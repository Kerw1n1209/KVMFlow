#ifndef M1DDC_SHIM_H
#define M1DDC_SHIM_H

// Port of m1ddc's service discovery + DDC/CI I2C layer.
// Source: https://github.com/waydabber/m1ddc (MIT License, (c) waydabber)
//
// Version provenance (the layer that matters - see 0.1.10):
//   discovery (primary): m1ddc v1.2.0 tag 2549fec, getDisplayAVService()
//     (sources/ioregistry.m) - the version installed via brew on the target
//     machine and the one that reads correct VCP 60 values there.
//   discovery (fallback): m1ddc master, getDisplayDDCTransport()
//     (framebuffer registry-ID matching; does not exist in 1.2.0) - kept only
//     as a fallback when the 1.2.0 walk finds nothing; selected variant is
//     reported via KvmM1DDCTransport.discovery.
//   transactions: byte-equivalent in 1.2.0 and master (verified by line
//     diff 2026-09-13); ported from master with the 1.2.0 single-byte decode
//     additionally exposed since 0.1.10.
// Ported 2026-09-13 for KVMFlow KVM-1. History: the 0.1.8 port carried the
// MASTER discovery while the on-machine reference was 1.2.0 - the two
// algorithms select different DCPAVServiceProxy instances on the dual-
// external-display target machine, which fully explains the canned-frame
// replies (0.1.9 ddc-raw A/B + 2026-09-13 link-order experiment).

#include <stdint.h>

typedef struct __IOAVService *IOAVServiceRef;

typedef struct {
    void *service;              // NULL when no external DCP proxy was found
    uint32_t chipAddress;       // 0x37 default, 0xB7 for MCDP29xx bridges
    uint64_t proxyRegistryEntryID; // selected DCPAVServiceProxy registry ID
    char proxyPath[1024];       // IORegistry path of the wrapped proxy
    char discovery[24];         // "1.2.0" | "fallback_master" | "none"
    void *ioService;            // lean mode: kept-alive io_service_t for the
                                // post-exchange evidence fill; NULL otherwise
} KvmM1DDCTransport;

// Discrimination-matrix options for ddc-raw (kvmprobe 0.1.11). They isolate
// the two remaining suspects after 0.1.10 ruled out proxy selection:
//   prelude - suspect A: replicate m1ddc 1.2.0 getOnlineDisplayInfos'
//             pre-DDC call chain (CGGetOnlineDisplayList, then per display
//             CoreDisplay_DisplayCreateInfoDictionary + CG*Number queries +
//             IORegistryEntryCopyFromPath) before the discovery walk;
//             preludeAttrs adds the adapter property reads (EDID UUID,
//             DisplayAttributes) as a sub-switch.
//   lean    - suspect B: zero IOKit calls between proxy selection and the
//             DDC exchange - no MCDP29xx parent probe, no GetPath /
//             GetRegistryEntryID on the selected proxy (evidence is filled
//             AFTER the exchange via kvm_m1ddc_fill_proxy_evidence); chip
//             defaults to 0x37 exactly as m1ddc 1.2.0 hardcodes.
typedef struct {
    int lean;
    int prelude;
    int preludeAttrs;
} KvmM1DDCOptions;

typedef struct {
    int ok;                     // 1 = the exchange completed
    int curValue;               // master decode: big-endian [8..9] of the 12-byte read
    int maxValue;               // master decode: big-endian [6..7]
    int curValueV120;           // m1ddc 1.2.0 decode: single byte [9]
    int maxValueV120;           // m1ddc 1.2.0 decode: single byte [7]
    uint8_t raw[12];            // the full reply buffer, for evidence logging
} KvmM1DDCValue;

KvmM1DDCTransport kvm_m1ddc_transport_for_display(uint32_t cgDisplayID);
KvmM1DDCTransport kvm_m1ddc_transport_for_display_opts(uint32_t cgDisplayID,
                                                       const KvmM1DDCOptions *opts);
// m1ddc 1.2.0 getOnlineDisplayInfos pre-DDC call chain (suspect A isolation);
// normally invoked via KvmM1DDCOptions.prelude, exposed for diagnostics.
void kvm_m1ddc_prelude(int withAttrs);
// Post-exchange forensic fill for lean mode: records the selected proxy's
// registry path/ID from the kept-alive io_service_t, then releases it.
// No-op when the transport was not built in lean mode.
void kvm_m1ddc_fill_proxy_evidence(KvmM1DDCTransport *transport);
KvmM1DDCValue kvm_m1ddc_get_vcp(void *service, uint32_t chipAddress, uint8_t attrCode);
int kvm_m1ddc_set_vcp(void *service, uint32_t chipAddress, uint8_t attrCode, uint16_t value);

// Pure selection core of the m1ddc 1.2.0 getDisplayAVService walk, exposed
// for selftest. Feed registry entries in iteration order; a step selects
// only a DCPAVServiceProxy-named entry with Location "External" that arrives
// AFTER the entry whose IOService-plane path equals the display's
// IODisplayLocation. Once armed the scan never rescopes (no sibling filter)
// and never re-arms (later path matches are ordinary entries), mirroring
// 1.2.0's "same iterator keeps walking" nested-loop semantics exactly.
typedef struct {
    char ioLocation[1024];      // copied at init
    int armed;                  // 0 = still scanning for the display node
} KvmV120Walk;

// Returns 0 on success; nonzero (walk stays inert) when ioLocation is NULL
// or too long, matching 1.2.0's ioLocation != NULL guard.
int kvm_v120_walk_init(KvmV120Walk *walk, const char *ioLocation);
void kvm_v120_walk_step(KvmV120Walk *walk, const char *path, const char *name,
                        const char *location, int *select);

#endif
