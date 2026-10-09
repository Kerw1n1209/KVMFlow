// Link-order isolation experiment for the KVM-1 DDC read defect.
//
// Background (2026-09-13): on the home-lab Mac, m1ddc reads VCP 60 as 15/7
// while kvmprobe 0.1.8/0.1.9 - running byte-identical transaction code via
// the m1ddcShim C port - receives a canned 12-byte frame from the same
// DCPAVServiceProxy registry entries within the same minute. otool shows the
// two processes load the relevant frameworks in opposite order:
//
//   /opt/homebrew/bin/m1ddc : CoreDisplay -> CoreGraphics -> IOKit
//   kvmprobe (SwiftPM)      : IOKit       -> CoreGraphics -> CoreDisplay
//                             (Package.swift linkerSettings order)
//
// This driver calls the SAME shim source (../../Sources/m1ddcShim) with no
// Swift runtime, no Foundation, no logging, and no CoreGraphics calls on the
// measurement path. build.sh compiles it twice; the two executables differ
// ONLY in framework link order. On the target machine that isolates the
// load/init-order variable:
//
//   linkorder-cd : -framework CoreDisplay -framework CoreGraphics -framework IOKit
//   linkorder-io : -framework IOKit       -framework CoreGraphics -framework CoreDisplay
//
// Usage:
//   linkorder-cd <cgDisplayID> [--vcp N] [--chip 0x37]   measurement (one DDC exchange)
//   linkorder-cd --list                                   1-based online display index -> CG ID map
//
// Exit codes match kvmprobe ddc-raw: 0 = exchange completed, 1 = exchange
// failed, 2 = no external AV service for this display. As with ddc-raw, a
// completed exchange says nothing about VCP decode validity - the raw bytes
// are the evidence.

#include "m1ddcShim.h"

#include <CoreGraphics/CoreGraphics.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#ifndef LINKORDER_DESC
#define LINKORDER_DESC "unknown (build without -DLINKORDER_DESC)"
#endif

static void usage(void) {
    fprintf(stderr,
            "usage: linkorder <cgDisplayID> [--vcp N] [--chip 0xHH]\n"
            "       linkorder --list\n");
    exit(64);
}

static int listOnlineDisplays(void) {
    CGDirectDisplayID ids[16];
    uint32_t count = 0;
    if (CGGetOnlineDisplayList(16, ids, &count) != kCGErrorSuccess) {
        fprintf(stderr, "error: CGGetOnlineDisplayList failed\n");
        return 1;
    }
    printf("link_order=%s\n", LINKORDER_DESC);
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

    printf("link_order=%s\n", LINKORDER_DESC);

    KvmM1DDCTransport t = kvm_m1ddc_transport_for_display((uint32_t)displayID);
    if (t.service == NULL) {
        printf("display_id=%lu result=error error=no_av_service_for_display\n", displayID);
        return 2;
    }

    uint32_t chip = chipOverride ? chipOverride : t.chipAddress;
    printf("display_id=%lu proxy_registry_entry_id=%llu proxy_registry_entry_id_hex=0x%llx\n",
           displayID, (unsigned long long)t.proxyRegistryEntryID,
           (unsigned long long)t.proxyRegistryEntryID);
    printf("proxy_path=%s\n", t.proxyPath);
    printf("chip_address=%u chip_address_hex=0x%x chip_address_overridden=%d\n",
           chip, chip, chipOverride ? 1 : 0);

    KvmM1DDCValue v = kvm_m1ddc_get_vcp(t.service, chip, vcp);
    if (!v.ok) {
        printf("exchange_ok=false vcp=%u\n", vcp);
        return 1;
    }

    printf("exchange_ok=true vcp=%u current=%d max=%d raw_reply=",
           vcp, v.curValue, v.maxValue);
    for (int i = 0; i < 12; i++) {
        printf("%02x%s", v.raw[i], i == 11 ? "" : " ");
    }
    printf("\n");
    printf("note=current/max_are_m1ddc_blind_decode_raw_bytes_are_the_evidence\n");
    return 0;
}
