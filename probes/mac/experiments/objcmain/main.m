// Process-shape isolation experiment - FINAL sidetrack round for the KVM-1
// DDC read defect. After the 0.1.11 four-cell matrix excluded both the
// m1ddc pre-DDC call chain (A) and our post-selection calls (B), the
// remaining suspect is C: the process-shape layer. This binary replicates
// m1ddc's ObjC-main process shape - ObjC entry point, @autoreleasepool,
// Foundation objects flowing through the path - while every IOAVService /
// IOKit / CoreDisplay / CoreGraphics call is made by the SAME shim source
// (../../Sources/m1ddcShim) used by kvmprobe ddc-raw and the linkorder
// binaries. It completes the three-level gradient:
//
//   pure C : experiments/linkorder/linkorder-cd/-io   -> canned frames (2026-09-13)
//   Swift  : kvmprobe ddc-raw 0.1.11 four-cell matrix -> canned frames (2026-09-13)
//   ObjC   : THIS binary (objcmain)                   -> real-machine verdict pending
//
// Call sequence == m1ddc 1.2.0 for `display <N> get input`:
//   kvm_m1ddc_prelude(1)                                (getOnlineDisplayInfos form,
//                                                        attribute reads included)
//   kvm_m1ddc_transport_for_display_opts(lean=1)        (1.2.0 walk, zero calls
//                                                        between create and exchange)
//   kvm_m1ddc_get_vcp                                   (the shared shim exchange)
//   kvm_m1ddc_fill_proxy_evidence                       (forensics AFTER the exchange)
//
// Exit codes match ddc-raw/linkorder: 0 = exchange completed, 1 = exchange
// failed, 2 = no external AV service. Raw bytes are the evidence; the
// decoded values carry no validity claim.

#import <Foundation/Foundation.h>
#import <CoreGraphics/CoreGraphics.h>
#include "m1ddcShim.h"
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#ifndef SHAPE_DESC
#define SHAPE_DESC "objc-main (unknown commit)"
#endif

static void usage(void) {
    fprintf(stderr, "usage: objcmain <cgDisplayID> [--vcp N] [--chip 0xHH]\n"
                    "       objcmain --list\n");
    exit(64);
}

static int listOnlineDisplays(void) {
    CGDirectDisplayID ids[16];
    uint32_t count = 0;
    if (CGGetOnlineDisplayList(16, ids, &count) != kCGErrorSuccess) {
        fprintf(stderr, "error: CGGetOnlineDisplayList failed\n");
        return 1;
    }
    NSString *shape = [NSString stringWithFormat:@"%s", SHAPE_DESC];
    printf("process_shape=%s\n", shape.UTF8String);
    for (uint32_t i = 0; i < count; i++) {
        printf("index=%u display_id=%u builtin=%d main=%d\n",
               i + 1, ids[i], CGDisplayIsBuiltin(ids[i]) != 0, CGDisplayIsMain(ids[i]) != 0);
    }
    return 0;
}

int main(int argc, char **argv) {
    if (argc < 2) usage();
    if (strcmp(argv[1], "--list") == 0) return listOnlineDisplays();

    char *end = NULL;
    unsigned long displayID = strtoul(argv[1], &end, 10);
    if (end == argv[1] || *end != '\0') usage();

    uint8_t vcp = 60;
    uint32_t chipOverride = 0;
    for (int i = 2; i < argc; i++) {
        if (strcmp(argv[i], "--vcp") == 0 && i + 1 < argc) {
            vcp = (uint8_t)strtoul(argv[++i], NULL, 10);
        } else if (strcmp(argv[i], "--chip") == 0 && i + 1 < argc) {
            const char *h = argv[++i];
            if (strncmp(h, "0x", 2) == 0) h += 2;
            chipOverride = (uint32_t)strtoul(h, NULL, 16);
        } else {
            usage();
        }
    }

    // m1ddc's process shape: ObjC main, autorelease pool, Foundation strings.
    @autoreleasepool {
        printf("process_shape=%s\n", [NSString stringWithFormat:@"%s", SHAPE_DESC].UTF8String);

        KvmM1DDCOptions opts;
        memset(&opts, 0, sizeof(opts));
        opts.prelude = 1;        // getOnlineDisplayInfos call-form before discovery
        opts.preludeAttrs = 1;   // m1ddc always reads EDID UUID + DisplayAttributes
        opts.lean = 1;           // zero calls between create and the exchange

        KvmM1DDCTransport t = kvm_m1ddc_transport_for_display_opts((uint32_t)displayID, &opts);
        if (t.service == NULL) {
            printf("display_id=%lu result=error error=no_av_service_for_display discovery=%s\n",
                   displayID, t.discovery[0] ? t.discovery : "none");
            return 2;
        }

        uint32_t chip = chipOverride ? chipOverride : t.chipAddress;
        KvmM1DDCValue v = kvm_m1ddc_get_vcp(t.service, chip, vcp);
        kvm_m1ddc_fill_proxy_evidence(&t);

        printf("display_id=%lu discovery=%s proxy_registry_entry_id=%llu proxy_registry_entry_id_hex=0x%llx\n",
               displayID, t.discovery, (unsigned long long)t.proxyRegistryEntryID,
               (unsigned long long)t.proxyRegistryEntryID);
        printf("proxy_path=%s\n", t.proxyPath);
        printf("chip_address=%u chip_address_hex=0x%x chip_address_overridden=%d "
               "m1ddc_prelude=true prelude_attrs=true lean=true\n",
               chip, chip, chipOverride ? 1 : 0);

        if (!v.ok) {
            printf("exchange_ok=false vcp=%u\n", vcp);
            return 1;
        }

        NSMutableString *hex = [NSMutableString string];
        for (int i = 0; i < 12; i++) {
            [hex appendFormat:@"%02x%s", v.raw[i], i == 11 ? "" : " "];
        }
        printf("exchange_ok=true vcp=%u current=%d max=%d current_v120=%d max_v120=%d\n",
               vcp, v.curValue, v.maxValue, v.curValueV120, v.maxValueV120);
        printf("raw_reply=%s\n", hex.UTF8String);
        printf("note=current/max_are_blind_decodes_raw_bytes_are_the_evidence\n");
        return 0;
    }
}
