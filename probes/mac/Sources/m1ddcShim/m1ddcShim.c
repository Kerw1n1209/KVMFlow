// Port of m1ddc's service discovery + DDC/CI I2C layer.
// Source: https://github.com/waydabber/m1ddc (MIT License, (c) waydabber)
//
// Version provenance (see include/m1ddcShim.h for the full story):
//   discovery primary  = m1ddc v1.2.0 tag 2549fec getDisplayAVService()
//   discovery fallback = m1ddc master getDisplayDDCTransport()
//   transactions       = master/1.2.0 byte-equivalent (line-diffed 2026-09-13)
//
// Transaction logic (packet building, checksum seeds, write counts, waits,
// read length) is kept byte-for-byte. The 1.2.0 discovery walk keeps m1ddc's
// exact iterator-order semantics, including its quirk of creating the
// IOAVServiceRef before checking the Location property; only object
// lifetime (releases) is added, which cannot change which proxy is chosen.

#include "include/m1ddcShim.h"

#include <CoreFoundation/CoreFoundation.h>
#include <CoreGraphics/CoreGraphics.h>
#include <IOKit/IOKitLib.h>
#include <stdbool.h>
#include <stdio.h>
#include <string.h>
#include <unistd.h>

// ---- m1ddc i2c.h constants (verbatim) ----
#define DEFAULT_INPUT_ADDRESS       0x51
#define ALTERNATE_INPUT_ADDRESS     0x50
#define DDC_WAIT                    10000   // us; up to 50000 on some displays
#define DDC_ITERATIONS              2
#define DDC_MCDP_READ_WAIT          50000   // us; 10 ms returned empty MCDP29xx replies
#define DDC_BUFFER_SIZE             256
#define DDC_CHIP_ADDRESS_DEFAULT    0x37
#define DDC_CHIP_ADDRESS_MCDP29XX   0xB7

#define STR_EQ(a, b) (strcmp((a), (b)) == 0)

// ---- private API externs (m1ddc declarations, verbatim) ----
extern CFDictionaryRef CoreDisplay_DisplayCreateInfoDictionary(uint32_t displayID);
extern IOAVServiceRef IOAVServiceCreateWithService(CFAllocatorRef allocator, io_service_t service);
extern IOReturn IOAVServiceReadI2C(IOAVServiceRef service, uint32_t chipAddress, uint32_t offset,
                                   void *outputBuffer, uint32_t outputBufferSize);
extern IOReturn IOAVServiceWriteI2C(IOAVServiceRef service, uint32_t chipAddress, uint32_t dataAddress,
                                    void *inputBuffer, uint32_t inputBufferSize);

// ---- m1ddc ioregistry.m helpers (verbatim semantics) ----

// m1ddc's getCFStringRef split in two: a raw create-rule recursive lookup
// (any CF type, caller releases) plus the string-typed wrapper m1ddc uses.
static CFTypeRef shim_getCFProperty(io_service_t service, const char *key) {
    CFStringRef keyStr = CFStringCreateWithCString(kCFAllocatorDefault, key, kCFStringEncodingASCII);
    if (keyStr == NULL) return NULL;
    CFTypeRef value = IORegistryEntrySearchCFProperty(service, kIOServicePlane, keyStr,
                                                      kCFAllocatorDefault, kIORegistryIterateRecursively);
    CFRelease(keyStr);
    return value;
}

static CFStringRef shim_getCFStringRef(io_service_t service, const char *key) {
    CFTypeRef value = shim_getCFProperty(service, key);
    if (value == NULL) return NULL;
    if (CFGetTypeID(value) != CFStringGetTypeID()) {
        CFRelease(value);
        return NULL;
    }
    return (CFStringRef)value;
}

// m1ddc's isMCDP29XXProxy (verbatim logic). NOTE: m1ddc 1.2.0 hardcodes chip
// 0x37 for every exchange; only master detects the MCDP29xx bridge. We keep
// the detection for both walks - it is a no-op on this target hardware
// (C->DP direct, no bridge) and strictly better on bridge wiring.
static Boolean shim_isMCDP29XXProxy(io_service_t proxy) {
    io_registry_entry_t parent = MACH_PORT_NULL;
    if (IORegistryEntryGetParentEntry(proxy, kIOServicePlane, &parent) != KERN_SUCCESS) {
        return false;
    }
    Boolean isMCDP29XX = false;
    CFStringRef providerClass = shim_getCFStringRef(parent, "EPICProviderClass");
    if (providerClass != NULL) {
        isMCDP29XX = CFStringCompare(providerClass, CFSTR("AppleDCPMCDP29XX"), 0) == kCFCompareEqualTo;
        CFRelease(providerClass);
    }
    IOObjectRelease(parent);
    return isMCDP29XX;
}

// m1ddc's getIORegistryRootIterator (verbatim)
static kern_return_t shim_getIORegistryRootIterator(io_iterator_t *iter) {
    io_registry_entry_t root = IORegistryGetRootEntry(kIOMainPortDefault);
    kern_return_t ret = IORegistryEntryCreateIterator(root, kIOServicePlane, kIORegistryIterateRecursively, iter);
    if (ret != KERN_SUCCESS && *iter != MACH_PORT_NULL) {
        IOObjectRelease(*iter);
    }
    return ret;
}

// Take ownership of `service` for the transport. Non-lean: record the proxy
// evidence (path, registry ID, MCDP chip) eagerly and release the
// io_service_t. Lean (suspect B isolation): ZERO calls on the selected proxy
// before the exchange - keep the port alive for the post-exchange fill and
// default the chip to 0x37, exactly as m1ddc 1.2.0 hardcodes.
static void shim_fillFromProxy(io_service_t service, IOAVServiceRef avService,
                               KvmM1DDCTransport *transport, bool lean) {
    transport->service = avService;
    if (lean) {
        transport->chipAddress = DDC_CHIP_ADDRESS_DEFAULT;
        transport->ioService = (void *)(uintptr_t)service;
        return;
    }
    IORegistryEntryGetPath(service, kIOServicePlane, transport->proxyPath);
    // Keep the exact proxy identity selected by the walk so the result can be
    // compared with a standalone m1ddc process (0.1.9 ddc-raw parity).
    IORegistryEntryGetRegistryEntryID(service, &transport->proxyRegistryEntryID);
    transport->chipAddress = shim_isMCDP29XXProxy(service)
        ? DDC_CHIP_ADDRESS_MCDP29XX
        : DDC_CHIP_ADDRESS_DEFAULT;
    IOObjectRelease(service);
}

// ---- pure 1.2.0 walk core (exposed for selftest) ----

int kvm_v120_walk_init(KvmV120Walk *walk, const char *ioLocation) {
    memset(walk, 0, sizeof(*walk));
    if (ioLocation == NULL || strlen(ioLocation) >= sizeof(walk->ioLocation)) {
        return 1;
    }
    strcpy(walk->ioLocation, ioLocation);
    return 0;
}

void kvm_v120_walk_step(KvmV120Walk *walk, const char *path, const char *name,
                        const char *location, int *select) {
    *select = 0;
    if (!walk->armed) {
        // 1.2.0 outer loop: only path equality against IODisplayLocation
        // arms the scan. (ioLocation was validated non-empty at init.)
        if (path != NULL && strcmp(path, walk->ioLocation) == 0) {
            walk->armed = 1;
        }
        return;
    }
    // 1.2.0 inner loop: the SAME iterator keeps walking - path is ignored,
    // the scan is not scoped to the display node's siblings, and the first
    // DCPAVServiceProxy with Location == "External" wins.
    if (name != NULL && STR_EQ(name, "DCPAVServiceProxy") &&
        location != NULL && STR_EQ(location, "External")) {
        *select = 1;
    }
}

// ---- m1ddc 1.2.0 (tag 2549fec) getDisplayAVService: IOKit driver ----
// Verbatim walk: single recursive root iterator; every entry's IOService
// path is compared to the display's IODisplayLocation; on the first equal
// entry the same iterator continues and the first DCPAVServiceProxy with
// Location "External" is wrapped with IOAVServiceCreateWithService. The
// AVServiceRef is created BEFORE the Location check, exactly as 1.2.0 does
// (non-qualifying proxies get released here instead of leaked - lifecycle
// only, no selection change).
static bool shim_v120_walk(CFStringRef ioLocation, KvmM1DDCTransport *transport, bool lean) {
    char locationBuf[1024];
    if (!CFStringGetCString(ioLocation, locationBuf, sizeof(locationBuf), kCFStringEncodingUTF8)) {
        return false;
    }
    KvmV120Walk walk;
    if (kvm_v120_walk_init(&walk, locationBuf) != 0) {
        return false;
    }

    io_iterator_t iter;
    if (shim_getIORegistryRootIterator(&iter) != KERN_SUCCESS) {
        return false;
    }

    io_service_t service;
    while ((service = IOIteratorNext(iter)) != MACH_PORT_NULL) {
        io_string_t servicePath;
        servicePath[0] = '\0';   // GetPath failure must yield "", never garbage
        IORegistryEntryGetPath(service, kIOServicePlane, servicePath);
        io_name_t name;
        name[0] = '\0';
        // m1ddc 1.2.0 queries names only after the display node armed the
        // scan (outer loop is path-only) - keep the same call pattern.
        if (walk.armed) {
            IORegistryEntryGetName(service, name);
        }

        // 1.2.0 reads "Location" (and creates the AV service) only for
        // DCPAVServiceProxy-named entries after the display node armed the
        // scan.
        IOAVServiceRef avService = NULL;
        CFStringRef locStr = NULL;
        if (walk.armed && STR_EQ(name, "DCPAVServiceProxy")) {
            avService = IOAVServiceCreateWithService(kCFAllocatorDefault, service);
            locStr = shim_getCFStringRef(service, "Location");
        }
        char locBuf[64];
        const char *locC = NULL;
        if (locStr != NULL &&
            CFStringGetCString(locStr, locBuf, sizeof(locBuf), kCFStringEncodingASCII)) {
            locC = locBuf;
        }

        int select = 0;
        kvm_v120_walk_step(&walk, servicePath, name, locC, &select);
        if (select && avService != NULL) {
            shim_fillFromProxy(service, avService, transport, lean);
            if (locStr != NULL) CFRelease(locStr);
            // Lean keeps the call surface between create and the exchange at
            // exactly zero kernel calls: even releasing the iterator is a
            // mach call, and m1ddc leaks it here. Short-lived process, so
            // the leaked port costs nothing.
            if (!lean) IOObjectRelease(iter);
            return true;
        }
        if (locStr != NULL) CFRelease(locStr);
        if (avService != NULL) CFRelease(avService);
        IOObjectRelease(service);
    }
    IOObjectRelease(iter);
    return false;
}

// ---- m1ddc master getDisplayDDCTransport: fallback walk ----
// Framebuffer registry-ID matching (this function does not exist in 1.2.0).
// Kept as the fallback for hardware where the 1.2.0 walk finds nothing;
// reported as discovery="fallback_master".
static bool shim_master_walk(CFStringRef ioLocation, KvmM1DDCTransport *transport, bool lean) {
    io_service_t adapter = IORegistryEntryCopyFromPath(kIOMainPortDefault, ioLocation);
    if (adapter == MACH_PORT_NULL) {
        return false;
    }

    uint64_t selectedAdapterID;
    kern_return_t idResult = IORegistryEntryGetRegistryEntryID(adapter, &selectedAdapterID);
    IOObjectRelease(adapter);
    if (idResult != KERN_SUCCESS) {
        return false;
    }

    io_iterator_t iter;
    if (shim_getIORegistryRootIterator(&iter) != KERN_SUCCESS) {
        return false;
    }

    Boolean framebufferMatchesDisplay = false;
    io_service_t service;
    while ((service = IOIteratorNext(iter)) != MACH_PORT_NULL) {
        if (IOObjectConformsTo(service, "IOMobileFramebuffer")) {
            uint64_t framebufferID;
            framebufferMatchesDisplay =
                IORegistryEntryGetRegistryEntryID(service, &framebufferID) == KERN_SUCCESS &&
                framebufferID == selectedAdapterID;
            IOObjectRelease(service);
            continue;
        }

        io_name_t name;
        IORegistryEntryGetName(service, name);
        if (!framebufferMatchesDisplay || !STR_EQ(name, "DCPAVServiceProxy")) {
            IOObjectRelease(service);
            continue;
        }

        IOAVServiceRef avService = IOAVServiceCreateWithService(kCFAllocatorDefault, service);
        if (avService == NULL) {
            IOObjectRelease(service);
            continue;
        }

        CFStringRef location = shim_getCFStringRef(service, "Location");
        Boolean isExternal = location != NULL &&
            CFStringCompare(CFSTR("External"), location, 0) == kCFCompareEqualTo;
        if (location != NULL) {
            CFRelease(location);
        }

        if (!isExternal) {
            CFRelease(avService);
            IOObjectRelease(service);
            continue;
        }

        shim_fillFromProxy(service, avService, transport, lean);
        // Same lean rule as the 1.2.0 walk: no kernel call after create.
        if (!lean) IOObjectRelease(iter);
        return true;
    }

    IOObjectRelease(iter);
    return false;
}

// ---- m1ddc 1.2.0 getOnlineDisplayInfos pre-DDC call chain (suspect A) ----
// The exact per-display sequence m1ddc runs before any DDC exchange:
// CGGetOnlineDisplayList, then for each display
// CoreDisplay_DisplayCreateInfoDictionary + CG{Serial,Model,Vendor}Number +
// IORegistryEntryCopyFromPath on IODisplayLocation. The adapter property
// reads (EDID UUID, DisplayAttributes) sit behind withAttrs as a sub-switch.
// Values are fetched and released; only the call pattern matters here.
void kvm_m1ddc_prelude(int withAttrs) {
    CGDirectDisplayID ids[16];   // m1ddc's MAX_DISPLAYS
    uint32_t count = 0;
    if (CGGetOnlineDisplayList(16, ids, &count) != kCGErrorSuccess) {
        return;
    }
    for (uint32_t i = 0; i < count; i++) {
        CFDictionaryRef info = CoreDisplay_DisplayCreateInfoDictionary(ids[i]);
        if (info == NULL) continue;
        (void)CGDisplaySerialNumber(ids[i]);
        (void)CGDisplayModelNumber(ids[i]);
        (void)CGDisplayVendorNumber(ids[i]);
        CFStringRef ioLocation = CFDictionaryGetValue(info, CFSTR("IODisplayLocation"));
        io_service_t adapter = MACH_PORT_NULL;
        if (ioLocation != NULL && CFGetTypeID(ioLocation) == CFStringGetTypeID()) {
            adapter = IORegistryEntryCopyFromPath(kIOMainPortDefault, ioLocation);
        }
        CFRelease(info);
        if (adapter == MACH_PORT_NULL) continue;
        if (withAttrs) {
            CFTypeRef v = shim_getCFProperty(adapter, "EDID UUID");
            if (v != NULL) CFRelease(v);
            v = shim_getCFProperty(adapter, "DisplayAttributes");
            if (v != NULL) CFRelease(v);
        }
        IOObjectRelease(adapter);
    }
}

// Top-level discovery: 1.2.0 walk first (matches the installed reference
// binary), master walk as fallback. The selected variant is reported in
// transport->discovery. opts selects the discrimination-matrix switches
// (prelude = suspect A, lean = suspect B); NULL means baseline.
KvmM1DDCTransport kvm_m1ddc_transport_for_display_opts(uint32_t cgDisplayID,
                                                       const KvmM1DDCOptions *opts) {
    KvmM1DDCOptions o;
    if (opts != NULL) {
        o = *opts;
    } else {
        memset(&o, 0, sizeof(o));
    }
    if (o.prelude) {
        kvm_m1ddc_prelude(o.preludeAttrs);
    }

    KvmM1DDCTransport transport;
    memset(&transport, 0, sizeof(transport));
    strcpy(transport.discovery, "none");

    CFDictionaryRef displayInfoDict = CoreDisplay_DisplayCreateInfoDictionary(cgDisplayID);
    if (displayInfoDict == NULL) {
        return transport;
    }
    CFStringRef ioLocation = (CFStringRef)CFDictionaryGetValue(displayInfoDict, CFSTR("IODisplayLocation"));
    if (ioLocation == NULL || CFGetTypeID(ioLocation) != CFStringGetTypeID()) {
        CFRelease(displayInfoDict);
        return transport;
    }
    CFRetain(ioLocation);
    CFRelease(displayInfoDict);

    if (shim_v120_walk(ioLocation, &transport, o.lean != 0)) {
        strcpy(transport.discovery, "1.2.0");
    } else if (shim_master_walk(ioLocation, &transport, o.lean != 0)) {
        strcpy(transport.discovery, "fallback_master");
    }
    CFRelease(ioLocation);
    return transport;
}

KvmM1DDCTransport kvm_m1ddc_transport_for_display(uint32_t cgDisplayID) {
    return kvm_m1ddc_transport_for_display_opts(cgDisplayID, NULL);
}

// Post-exchange forensic fill for lean mode: records the selected proxy's
// registry path/ID from the kept-alive io_service_t, then releases it.
void kvm_m1ddc_fill_proxy_evidence(KvmM1DDCTransport *transport) {
    if (transport->ioService == NULL) return;
    io_service_t service = (io_service_t)(uintptr_t)transport->ioService;
    IORegistryEntryGetPath(service, kIOServicePlane, transport->proxyPath);
    IORegistryEntryGetRegistryEntryID(service, &transport->proxyRegistryEntryID);
    IOObjectRelease(service);
    transport->ioService = NULL;
}

// ---- m1ddc i2c.m transaction layer (verbatim) ----

// m1ddc's getBytesUsed: highest nonzero byte index + 1
static int shim_getBytesUsed(uint8_t *data) {
    int bytes = 0;
    for (int i = 0; i < DDC_BUFFER_SIZE; ++i) {
        if (data[i] != 0) {
            bytes = i + 1;
        }
    }
    return bytes;
}

// m1ddc's prepareDDCRead: [0x82, 0x01, code, chk] with 0x6E seed
static void shim_prepareDDCRead(uint8_t *data) {
    data[0] = 0x82;
    data[1] = 0x01;
    data[3] = 0x6e ^ data[0] ^ data[1] ^ data[2] ^ data[3];
}

// m1ddc's prepareDDCWrite: [0x84, 0x03, code, hi, lo, chk], seed 0x6E + sub-address
static void shim_prepareDDCWrite(uint8_t *data, uint16_t newValue) {
    data[0] = 0x84;
    data[1] = 0x03;
    data[3] = (uint8_t)(newValue >> 8);
    data[4] = (uint8_t)(newValue & 255);
    data[5] = 0x6E ^ DEFAULT_INPUT_ADDRESS ^ data[0] ^ data[1] ^ data[2] ^ data[3] ^ data[4];
}

// m1ddc's performDDCWriteAtChipAddress: command written DDC_ITERATIONS
// times, each preceded by a DDC_WAIT sleep
static IOReturn shim_performDDCWriteAtChipAddress(IOAVServiceRef avService, uint32_t chipAddress,
                                                  uint8_t *data) {
    IOReturn ret = KERN_SUCCESS;
    for (int i = 0; i < DDC_ITERATIONS; ++i) {
        usleep(DDC_WAIT);
        ret = IOAVServiceWriteI2C(avService, chipAddress, DEFAULT_INPUT_ADDRESS, data,
                                  (uint32_t)shim_getBytesUsed(data));
        if (ret) {
            return ret;
        }
    }
    return ret;
}

// m1ddc's performDDCReadAtChipAddress: 12-byte read from sub-address 0x51
// after a chip-dependent wait (50 ms on MCDP29xx, 10 ms otherwise)
static IOReturn shim_performDDCReadAtChipAddress(IOAVServiceRef avService, uint32_t chipAddress,
                                                 uint8_t *data) {
    memset(data, 0, DDC_BUFFER_SIZE);
    usleep(chipAddress == DDC_CHIP_ADDRESS_MCDP29XX ? DDC_MCDP_READ_WAIT : DDC_WAIT);
    return IOAVServiceReadI2C(avService, chipAddress, DEFAULT_INPUT_ADDRESS, data, 12);
}

KvmM1DDCValue kvm_m1ddc_get_vcp(void *service, uint32_t chipAddress, uint8_t attrCode) {
    IOAVServiceRef avService = (IOAVServiceRef)service;
    KvmM1DDCValue out;
    memset(&out, 0, sizeof(out));

    uint8_t buffer[DDC_BUFFER_SIZE];
    memset(buffer, 0, sizeof(buffer));
    buffer[2] = attrCode;   // m1ddc's createDDCPacket

    shim_prepareDDCRead(buffer);
    IOReturn err = shim_performDDCWriteAtChipAddress(avService, chipAddress, buffer);
    if (err) {
        return out;
    }

    uint8_t readBuffer[DDC_BUFFER_SIZE];
    memset(readBuffer, 0, sizeof(readBuffer));
    err = shim_performDDCReadAtChipAddress(avService, chipAddress, readBuffer);
    if (err) {
        return out;
    }

    memcpy(out.raw, readBuffer, 12);
    // m1ddc master convertI2CtoDDC: current = big-endian [8..9], max = [6..7]
    out.maxValue = (readBuffer[6] << 8) | readBuffer[7];
    out.curValue = (readBuffer[8] << 8) | readBuffer[9];
    // m1ddc 1.2.0 convertI2CtoDDC: single bytes [7] (max) and [9] (current)
    out.maxValueV120 = readBuffer[7];
    out.curValueV120 = readBuffer[9];
    out.ok = 1;
    return out;
}

int kvm_m1ddc_set_vcp(void *service, uint32_t chipAddress, uint8_t attrCode, uint16_t value) {
    IOAVServiceRef avService = (IOAVServiceRef)service;
    uint8_t buffer[DDC_BUFFER_SIZE];
    memset(buffer, 0, sizeof(buffer));
    buffer[2] = attrCode;

    shim_prepareDDCWrite(buffer, value);
    IOReturn err = shim_performDDCWriteAtChipAddress(avService, chipAddress, buffer);
    return err == KERN_SUCCESS ? 0 : 1;
}
