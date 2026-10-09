import Foundation
import CoreGraphics
import IOKit
import m1ddcShim

// DDC/CI on Apple Silicon goes through the DCP display pipe. Since 0.1.8 the
// service discovery AND the I2C transaction layer are a port of m1ddc
// (MIT, (c) waydabber) in Sources/m1ddcShim. 0.1.10 corrected the discovery
// provenance: the primary walk now replicates m1ddc v1.2.0 tag 2549fec's
// getDisplayAVService (the brew-stable version installed on the target
// machine, which reads correct values there) - the 0.1.8 port had carried
// the MASTER discovery (framebuffer registry-ID matching), and the two
// algorithms select different proxies on the dual-external-display target.
// The Swift extern below is only used by the post-DDC EDID fallback read.
@_silgen_name("IOAVServiceReadI2C")
func ioAVServiceReadI2C(_ service: UnsafeMutableRawPointer?, _ chipAddress: UInt32, _ offset: UInt32,
                        _ buffer: UnsafeMutableRawPointer, _ length: UInt32) -> Int32

private let kIOServicePlaneC = "IOService"
let ddcCiSubAddress: UInt32 = 0x51
private let edidChipAddress: UInt32 = 0x50

enum DdcError: Error, CustomStringConvertible {
    case noService
    case noDisplayLocation
    case edidUnavailable
    case writeFailed(rc: Int32)
    case readFailed(rc: Int32)
    case badReply(String)
    var description: String {
        switch self {
        case .noService: return "no_av_service_for_display"
        case .noDisplayLocation: return "IODisplayLocation_not_found"
        case .edidUnavailable: return "edid_unavailable"
        case .writeFailed(let rc): return "i2c_write_failed rc=\(rc)"
        case .readFailed(let rc): return "i2c_read_failed rc=\(rc)"
        case .badReply(let m): return m
        }
    }
}

// MARK: - AV service discovery (m1ddc shim)

struct AvDisplayService {
    let ref: UnsafeMutableRawPointer   // IOAVServiceRef; probe process is short-lived, no release
    let chipAddress: UInt32
    let proxyRegistryEntryID: UInt64
    let proxyPath: String
    let discovery: String              // shim walk that selected the proxy: "1.2.0" | "fallback_master" | "none"
}

func findAvService(for displayID: CGDirectDisplayID) -> AvDisplayService? {
    let t = kvm_m1ddc_transport_for_display(displayID)
    guard let service = t.service else { return nil }
    let path = withUnsafeBytes(of: t.proxyPath) { raw in
        raw.baseAddress.flatMap { String(cString: $0.assumingMemoryBound(to: CChar.self)) } ?? ""
    }
    let discovery = withUnsafeBytes(of: t.discovery) { raw in
        raw.baseAddress.flatMap { String(cString: $0.assumingMemoryBound(to: CChar.self)) } ?? "none"
    }
    return AvDisplayService(ref: service, chipAddress: t.chipAddress,
                            proxyRegistryEntryID: t.proxyRegistryEntryID, proxyPath: path,
                            discovery: discovery)
}

// MARK: - DDC/CI exchange

func ddcChecksum(_ bytes: ArraySlice<UInt8>) -> UInt8 {
    var c: UInt8 = 0
    for b in bytes { c ^= b }
    return c
}

struct VcpReading {
    let code: Int
    let current: Int
    let max: Int
    let rawHex: String
    var attempts: Int
}

struct VcpDiagnostics {
    var chipAddress: UInt32 = 0
    var attempts = 0
    var lastError = "no_attempt"
    var lastRawHex: String? = nil
    var triedAlternateChip = false
    /// per-attempt outcome, e.g. "#1 0x37: i2c_read_failed rc=-536870905" or
    /// "#3 0x37: unvalidated[6e 88 ...]" — the full ladder is preserved so a
    /// failure event shows what EACH address returned, not just the last try
    var attemptLog: [String] = []

    mutating func record(chip: UInt32, error: String, rawHex: String?) {
        chipAddress = chip
        lastError = error
        lastRawHex = rawHex
        if attemptLog.count < 12 {
            attemptLog.append("#\(attemptLog.count + 1) 0x\(String(chip, radix: 16)): \(rawHex != nil ? "unvalidated[\(rawHex!)]" : error)")
        }
    }
}

enum VcpOutcome {
    case ok(VcpReading, chipAddress: UInt32)
    case failed(VcpDiagnostics)
}

enum VcpParseError: Error, CustomStringConvertible {
    case tooShort(String)
    case nullMessage(String)
    case badHeader(String)
    case checksum(String)
    var description: String {
        switch self {
        case .tooShort(let m): return "reply_too_short[\(m)]"
        case .nullMessage(let m): return "ddc_null_message[\(m)]"
        case .badHeader(let m): return "bad_reply_header[\(m)]"
        case .checksum(let m): return "reply_checksum_mismatch[\(m)]"
        }
    }
}

/// Pure reply parser, unit-tested by `kvmprobe selftest`.
/// Standard Get VCP Feature Reply frame: [6E][8E][code][mh][ml][sh][sl][chk],
/// but dialects exist: the length byte can be 0x8x other than 0x8E, and helper
/// bytes may sit between the length and the code echo (observed shape:
/// [6E][88][02][01][3C][mh][ml][sh][sl][chk]). The frame signature [6E][8x] is
/// scanned at offsets 0...4 (IOAVService read buffers may carry pad bytes and,
/// after an EDID read on the same bus, stale trailing bytes); the code echo is
/// tried at +2/+3/+4 and every candidate is gated on the DDC/CI XOR checksum,
/// so garbage cannot parse.
func parseVcpReply(_ buf: [UInt8], code: UInt8) -> Result<VcpReading, VcpParseError> {
    let hex = buf.map { String(format: "%02x", $0) }.joined(separator: " ")
    guard buf.count >= 8 else { return .failure(.tooShort(hex)) }
    // DDC/CI Standard v1.1 §6.4 null message: 6E 80 (length 0) — the monitor
    // answered the bus but has no payload to give; classified distinctly so the
    // session log separates "protocol-level refusal" from "garbage header".
    if buf[0] == 0x6E && buf[1] == 0x80 { return .failure(.nullMessage(hex)) }
    var sawFrameHeader = false
    let maxScan = min(4, buf.count - 8)
    for off in 0...maxScan {
        guard buf[off] == 0x6E, buf[off + 1] & 0x80 == 0x80 else { continue }
        sawFrameHeader = true
        for codePos in [off + 2, off + 3, off + 4] {
            guard codePos + 5 < buf.count, buf[codePos] == code else { continue }
            let chkPos = codePos + 5
            guard ddcChecksum(buf[off..<chkPos]) == buf[chkPos] else { continue }
            let maxVal = (Int(buf[codePos + 1]) << 8) | Int(buf[codePos + 2])
            let cur = (Int(buf[codePos + 3]) << 8) | Int(buf[codePos + 4])
            return .success(VcpReading(code: Int(code), current: cur, max: maxVal, rawHex: hex, attempts: 1))
        }
    }
    if sawFrameHeader { return .failure(.checksum(hex)) }
    return .failure(.badHeader(hex))
}

/// One Get VCP exchange via the ported m1ddc transaction (Sources/m1ddcShim).
/// The shim performs the full m1ddc sequence internally: [82 01 code chk]
/// written twice 10 ms apart, 12-byte read from sub-address 0x51, values
/// decoded at m1ddc's offsets (cur=[8..9], max=[6..7]). The raw 12 bytes are
/// kept for evidence logging. A fully-degenerate decode (cur=0 AND max=0) is
/// reported as a failure with the raw frame attached rather than as a reading.
private func ddcGetVcpAttempt(svc: AvDisplayService, code: UInt8, diag: inout VcpDiagnostics) -> VcpReading? {
    diag.attempts += 1
    diag.chipAddress = svc.chipAddress
    let r = kvm_m1ddc_get_vcp(svc.ref, svc.chipAddress, code)
    let hex = withUnsafeBytes(of: r.raw) { Array($0).map { String(format: "%02x", $0) }.joined(separator: " ") }
    guard r.ok != 0 else {
        diag.record(chip: svc.chipAddress, error: "m1ddc_exchange_failed(i2c_error)", rawHex: hex)
        return nil
    }
    guard r.curValue != 0 || r.maxValue != 0 else {
        diag.record(chip: svc.chipAddress, error: "degenerate_decode[cur=0 max=0]", rawHex: hex)
        return nil
    }
    return VcpReading(code: Int(code), current: Int(r.curValue), max: Int(r.maxValue),
                      rawHex: hex, attempts: diag.attempts)
}

/// Retry ladder on the shim-selected chip address. Each attempt is one full
/// m1ddc transaction; the ladder only adds m1ddc-style retries with spacing.
func ddcGetVcpFull(_ svc: AvDisplayService, code: UInt8) -> VcpOutcome {
    var diag = VcpDiagnostics()
    let interAttemptDelays: [useconds_t] = [0, 150_000, 300_000]
    for i in 0..<interAttemptDelays.count {
        usleep(interAttemptDelays[i])
        if let r = ddcGetVcpAttempt(svc: svc, code: code, diag: &diag) {
            return .ok(r, chipAddress: svc.chipAddress)
        }
    }
    return .failed(diag)
}

/// Set VCP exchange via the ported m1ddc write transaction
/// ([84 03 code hi lo chk], written twice 10 ms apart).
func ddcSetVcp(_ svc: AvDisplayService, code: UInt8, value: Int) throws {
    let rc = kvm_m1ddc_set_vcp(svc.ref, svc.chipAddress, code, UInt16(clamping: value))
    guard rc == 0 else { throw DdcError.writeFailed(rc: Int32(rc)) }
}

/// I2C EDID read at 0x50 — PROVEN to poison subsequent 0x51 DDC/CI reads on
/// this DCP path (real-hardware evidence 2026-09-13: canned frame with EDID
/// residue from both monitors). Callers MUST only use this AFTER all DDC
/// transactions for the display are complete, as the last-resort identity
/// source. Prefer readEdidFromRegistry.
func readEdidViaI2C(_ svc: AvDisplayService) throws -> [UInt8] {
    var edid = [UInt8](repeating: 0, count: 128)
    let rc = edid.withUnsafeMutableBytes { buf in
        ioAVServiceReadI2C(svc.ref, edidChipAddress, 0, buf.baseAddress!, 128)
    }
    guard rc == KERN_SUCCESS else { throw DdcError.edidUnavailable }
    guard edid[0] == 0x00, edid[1] == 0xFF else { throw DdcError.edidUnavailable }
    return edid
}

/// EDID from the IORegistry — zero I2C traffic, like m1ddc (which sources all
/// display data from the registry). Walks the DCP proxy's ancestor chain
/// looking for an IODisplayEDID data property, using only the explicit
/// Unmanaged create-rule API. The 0.1.6 recursive IORegistryEntrySearchCFProperty
/// variant and the dlsym'd IOAVServiceCopyEDID source were REMOVED in 0.1.7
/// after a real-hardware SIGSEGV: neither path can execute on a builtin-only
/// dev machine, and an unknown-ABI call plus an audited (ownership-ambiguous)
/// CF return are exactly the kind of code that only fails on first contact
/// with a real external display.
func readEdidFromRegistry(proxyPath: String) -> [UInt8]? {
    func validEdid(_ data: Data) -> [UInt8]? {
        let b = [UInt8](data.prefix(128))
        return (b.count == 128 && b[0] == 0x00 && b[1] == 0xFF) ? b : nil
    }
    let entry = proxyPath.withCString { IORegistryEntryFromPath(kIOMainPortDefault, $0) }
    guard entry != 0 else { return nil }
    defer { IOObjectRelease(entry) }

    var toRelease: [io_registry_entry_t] = []
    defer { for e in toRelease { IOObjectRelease(e) } }
    var current = entry
    for _ in 0..<10 {
        if let cf = IORegistryEntryCreateCFProperty(current, "IODisplayEDID" as CFString, kCFAllocatorDefault, 0)?
            .takeRetainedValue() as? Data, let b = validEdid(cf) { return b }
        var parent: io_registry_entry_t = 0
        guard IORegistryEntryGetParentEntry(current, kIOServicePlaneC, &parent) == KERN_SUCCESS,
              parent != 0, parent != current else { break }
        toRelease.append(parent)
        current = parent
    }
    return nil
}

/// Last-resort chain, callable only AFTER DDC work: the registry source was
/// already tried before the DDC ladder; the only remaining fallback is the
/// (proven) I2C read, which is safe once no DDC transaction follows it.
func resolveEdidPostDdc(_ svc: AvDisplayService) -> (bytes: [UInt8], source: String)? {
    if let b = try? readEdidViaI2C(svc) { return (b, "i2c_after_ddc") }
    return nil
}

// CoreDisplay private API for the identity-fallback dictionary (create rule)
@_silgen_name("CoreDisplay_DisplayCreateInfoDictionary")
func coreDisplayCreateInfoDictionary(_ displayID: CGDirectDisplayID) -> Unmanaged<CFDictionary>?

// MARK: - probing

struct DisplayReport {
    var json: [String: Any]
}

func enumerateDisplays() -> [CGDirectDisplayID] {
    var ids = [CGDirectDisplayID](repeating: 0, count: 16)
    var count: UInt32 = 0
    CGGetActiveDisplayList(16, &ids, &count)
    return Array(ids[0..<Int(count)])
}

func displayProbe(log: SessionLog) -> [DisplayReport] {
    let ids = enumerateDisplays()
    log.event("display.enumerate", ["count": ids.count])
    var reports: [DisplayReport] = []

    for id in ids {
        let builtin = CGDisplayIsBuiltin(id) == 1
        let bounds = CGDisplayBounds(id)
        var info: [String: Any] = [
            "display_id": Int(id),
            "builtin": builtin,
            "main": CGDisplayIsMain(id) == 1,
            "online": CGDisplayIsActive(id) == 1,
            "width": Int(bounds.width),
            "height": Int(bounds.height),
            "cg_vendor": Int(CGDisplayVendorNumber(id)),
            "cg_model": Int(CGDisplayModelNumber(id)),
            "cg_serial": Int(CGDisplaySerialNumber(id)),
        ]

        // CoreDisplay info dictionary: identity fallback + IORegistry location
        if let dictPtr = coreDisplayCreateInfoDictionary(id) {
            let cd = dictPtr.takeRetainedValue() as NSDictionary
            info["display_vendor_id"] = cd["DisplayVendorID"]
            info["display_product_id"] = cd["DisplayProductID"]
            info["display_serial"] = cd["DisplaySerialNumber"]
            if let loc = cd["IODisplayLocation"] as? String { info["display_location"] = loc }
        }

        if builtin {
            info["vcp60_read"] = ["supported": false, "reason": "skipped_builtin_display"] as [String: Any]
            log.event("display.info", ["display_id": Int(id), "builtin": builtin])
            log.event("display.ddc.read", [
                "display_id": Int(id), "vcp": 60, "result": "skipped",
                "error": "builtin_display_has_no_ddc",
            ])
            reports.append(DisplayReport(json: info))
            continue
        }

        if let svc = findAvService(for: id) {
            info["av_service_chip_address"] = Int(svc.chipAddress)
            info["av_service_proxy"] = svc.proxyPath

            // EDID source 1: IORegistry — no I2C. A 0x50 sub-read on this
            // service BEFORE the DDC ladder poisons subsequent 0x51 reads with
            // canned DCP-buffer frames (real-hardware evidence), so it is
            // forbidden from this point on for this display.
            var edidBytes = readEdidFromRegistry(proxyPath: svc.proxyPath)
            var edidSource = edidBytes != nil ? "ioregistry" : "none"

            // VCP60 ladder with a clean bus — runs regardless of EDID outcome
            let outcome = ddcGetVcpFull(svc, code: 60)

            // EDID source 2, post-DDC only (the proven I2C read — safe once
            // no DDC transaction follows). Every source degrades; total
            // failure leaves edid_source=none and the probe continues.
            if edidBytes == nil, let r = resolveEdidPostDdc(svc) {
                edidBytes = r.bytes
                edidSource = r.source
            }

            var edidOk = false
            if let edid = edidBytes, let parsed = try? parseEdid(edid) {
                edidOk = parsed.checksumOk
                info["edid_source"] = edidSource
                info["edid_id"] = parsed.edidId
                info["manufacturer"] = parsed.manufacturer
                info["product_code"] = String(format: "%04X", parsed.productCode)
                info["serial_number"] = Int(parsed.serialNumber)
                info["model_name"] = parsed.modelName ?? ""
                info["serial_string"] = parsed.serialString ?? ""
                info["edid_year"] = parsed.year
                info["edid_checksum_ok"] = parsed.checksumOk
                info["edid_hex"] = parsed.hex
                log.event("display.info", [
                    "display_id": Int(id), "edid_id": parsed.edidId,
                    "model_name": parsed.modelName ?? "", "manufacturer": parsed.manufacturer,
                    "edid_source": edidSource,
                ])
            } else {
                info["edid_source"] = "none"
                log.event("display.info", ["display_id": Int(id), "edid": "unavailable", "edid_source": "none"])
            }

            switch outcome {
            case .ok(let r, let chip):
                info["vcp60_read"] = [
                    "supported": true, "current": r.current, "max": r.max,
                    "attempts": r.attempts, "raw": r.rawHex, "chip_address": Int(chip),
                ] as [String: Any]
                log.event("display.ddc.read", [
                    "display_id": Int(id), "vcp": 60, "result": "ok",
                    "current": r.current, "max": r.max, "attempts": r.attempts,
                    "raw_reply": r.rawHex, "chip_address": Int(chip),
                ])
            case .failed(let d):
                info["vcp60_read"] = [
                    "supported": false, "reason": d.lastError,
                    "attempts": d.attempts, "chip_address": Int(d.chipAddress),
                    "tried_alternate_chip": d.triedAlternateChip,
                    "attempts_detail": d.attemptLog,
                    "edid_readable": edidOk,
                ] as [String: Any]
                var fields: [String: Any] = [
                    "display_id": Int(id), "vcp": 60, "result": "error",
                    "error": d.lastError, "attempts": d.attempts,
                    "chip_address": Int(d.chipAddress),
                    "tried_alternate_chip": d.triedAlternateChip,
                    "attempts_detail": d.attemptLog,
                    "edid_readable": edidOk,
                ]
                if let raw = d.lastRawHex { fields["raw_reply"] = raw }
                log.event("display.ddc.read", fields)
            }
        } else {
            info["vcp60_read"] = ["supported": false, "reason": DdcError.noService.description] as [String: Any]
            log.event("display.info", ["display_id": Int(id), "av_service": "unavailable"])
            log.event("display.ddc.read", [
                "display_id": Int(id), "vcp": 60, "result": "error",
                "error": DdcError.noService.description,
            ])
        }
        reports.append(DisplayReport(json: info))
    }
    return reports
}

func runDdcGet(displayID: CGDirectDisplayID, vcp: UInt8, chipOverride: UInt32?, log: SessionLog) -> Int32 {
    guard let found = findAvService(for: displayID) else {
        log.event("display.ddc.read", ["display_id": Int(displayID), "vcp": Int(vcp),
                                       "result": "error", "error": DdcError.noService.description])
        return 2
    }
    let svc = chipOverride.map {
        AvDisplayService(ref: found.ref, chipAddress: $0,
                         proxyRegistryEntryID: found.proxyRegistryEntryID, proxyPath: found.proxyPath,
                         discovery: found.discovery)
    } ?? found
    if let chip = chipOverride {
        log.event("display.ddc.read", ["display_id": Int(displayID), "note": "chip_address_overridden",
                                       "chip_address": Int(chip)])
    }
    switch ddcGetVcpFull(svc, code: vcp) {
    case .ok(let r, let chip):
        printJSON(["display_id": Int(displayID), "vcp": r.code, "current": r.current,
                   "max": r.max, "attempts": r.attempts, "raw_reply": r.rawHex,
                   "chip_address": Int(chip)])
        log.event("display.ddc.read", ["display_id": Int(displayID), "vcp": Int(vcp), "result": "ok",
                                       "current": r.current, "max": r.max, "attempts": r.attempts,
                                       "raw_reply": r.rawHex, "chip_address": Int(chip)])
        return 0
    case .failed(let d):
        // edid_readable discriminator, post-DDc chain (registry was not yet
        // tried in ddc-get — nothing was read before the DDC attempt here)
        var edidOk = false
        if let b = readEdidFromRegistry(proxyPath: svc.proxyPath) {
            edidOk = (try? parseEdid(b))?.checksumOk ?? false
        } else if let r = resolveEdidPostDdc(svc), let parsed = try? parseEdid(r.bytes) {
            edidOk = parsed.checksumOk
        }
        var fields: [String: Any] = [
            "display_id": Int(displayID), "vcp": Int(vcp), "result": "error",
            "error": d.lastError, "attempts": d.attempts,
            "chip_address": Int(d.chipAddress), "tried_alternate_chip": d.triedAlternateChip,
            "attempts_detail": d.attemptLog,
            "edid_readable": edidOk,
        ]
        if let raw = d.lastRawHex { fields["raw_reply"] = raw }
        log.event("display.ddc.read", fields)
        fputs("error: \(d.lastError) (attempts=\(d.attempts), chip=0x\(String(d.chipAddress, radix: 16)), edid_readable=\(edidOk))\n", stderr)
        for line in d.attemptLog { fputs("  \(line)\n", stderr) }
        return 1
    }
}

/// Read a NUL-terminated C char array field out of a C struct.
func cStringField<T>(_ field: T) -> String {
    withUnsafeBytes(of: field) { raw in
        raw.baseAddress.flatMap { String(cString: $0.assumingMemoryBound(to: CChar.self)) } ?? ""
    }
}

/// Minimal process-level reproduction for comparing the shim with a
/// standalone m1ddc binary. It intentionally performs only m1ddc-compatible
/// service discovery and one DDC exchange: no display enumeration, EDID
/// reads, parser, retry ladder, or post-failure IOKit calls. The returned raw
/// buffer is always emitted as a 12-byte hex string when the exchange itself
/// completed; no claim is made that the bytes contain a valid VCP value.
///
/// 0.1.11 discrimination-matrix switches (composable; each output row is
/// tagged with its cell):
///   lean        - suspect B: zero IOKit calls between proxy selection and
///                 the exchange; proxy path/ID evidence is filled after it;
///   prelude     - suspect A: replicate m1ddc's getOnlineDisplayInfos call
///                 chain before the discovery walk;
///   preludeAttrs- prelude sub-switch adding the adapter property reads.
func runDdcRaw(displayID: CGDirectDisplayID, vcp: UInt8, chipOverride: UInt32?, lean: Bool,
               prelude: Bool, preludeAttrs: Bool, log: SessionLog) -> Int32 {
    var opts = KvmM1DDCOptions()
    opts.lean = lean ? 1 : 0
    opts.prelude = prelude ? 1 : 0
    opts.preludeAttrs = preludeAttrs ? 1 : 0
    var t = kvm_m1ddc_transport_for_display_opts(displayID, &opts)

    guard let svcRef = t.service else {
        let variant = cStringField(t.discovery)
        let fields: [String: Any] = [
            "display_id": Int(displayID), "vcp": Int(vcp), "result": "error",
            "error": DdcError.noService.description,
            "discovery": variant.isEmpty ? "none" : variant,
            "transaction": "m1ddc_shim_single_exchange",
            "lean": lean, "m1ddc_prelude": prelude, "prelude_attrs": preludeAttrs,
        ]
        log.event("display.ddc.raw", fields)
        printJSON(fields)
        return 2
    }

    let chip = chipOverride ?? t.chipAddress
    let result = kvm_m1ddc_get_vcp(svcRef, chip, vcp)
    if lean {
        // Post-exchange forensic fill; nothing touched the proxy in between.
        kvm_m1ddc_fill_proxy_evidence(&t)
    }
    let rawHex = withUnsafeBytes(of: result.raw) {
        Array($0).map { String(format: "%02x", $0) }.joined(separator: " ")
    }
    let exchangeOK = result.ok != 0
    let variant = cStringField(t.discovery)
    let fields: [String: Any] = [
        "display_id": Int(displayID),
        "vcp": Int(vcp),
        "result": exchangeOK ? "ok" : "error",
        "exchange_ok": exchangeOK,
        "discovery": variant.isEmpty ? "none" : variant,
        "transaction": "m1ddc_shim_single_exchange",
        "lean": lean,
        "m1ddc_prelude": prelude,
        "prelude_attrs": preludeAttrs,
        "proxy_registry_entry_id": NSNumber(value: t.proxyRegistryEntryID),
        "proxy_registry_entry_id_hex": "0x" + String(t.proxyRegistryEntryID, radix: 16),
        "proxy_path": cStringField(t.proxyPath),
        "chip_address": Int(chip),
        "chip_address_overridden": chipOverride != nil,
        "current": exchangeOK ? result.curValue : NSNull(),
        "max": exchangeOK ? result.maxValue : NSNull(),
        "current_v120": exchangeOK ? result.curValueV120 : NSNull(),
        "max_v120": exchangeOK ? result.maxValueV120 : NSNull(),
        "raw_reply": exchangeOK ? rawHex : NSNull(),
        "raw_reply_valid": exchangeOK,
    ]
    log.event("display.ddc.raw", fields)
    printJSON(fields)
    if !exchangeOK {
        fputs("error: m1ddc single exchange failed (proxy=0x\(String(t.proxyRegistryEntryID, radix: 16)), chip=0x\(String(chip, radix: 16)))\n", stderr)
        return 1
    }
    return 0
}

/// NOTE: this changes the monitor's actual input source. Only run during the human
/// hardware session with the operator ready to confirm the picture and restore it.
func runDdcSet(displayID: CGDirectDisplayID, vcp: UInt8, value: Int, log: SessionLog) -> Int32 {
    guard let svc = findAvService(for: displayID) else {
        log.event("display.ddc.write", ["display_id": Int(displayID), "vcp": Int(vcp), "value": value,
                                        "result": "error", "error": DdcError.noService.description])
        return 2
    }
    var before: Int? = nil
    if case .ok(let r, _) = ddcGetVcpFull(svc, code: vcp) { before = r.current }
    do {
        try ddcSetVcp(svc, code: vcp, value: value)
        usleep(300_000)
        var after: Int? = nil
        var afterDiag: String? = nil
        switch ddcGetVcpFull(svc, code: vcp) {
        case .ok(let r, _): after = r.current
        case .failed(let d): afterDiag = d.lastError
        }
        log.event("display.ddc.write", [
            "display_id": Int(displayID), "vcp": Int(vcp), "value": value,
            "result": "ok", "previous": before ?? -1, "read_back": after ?? -1,
            "read_back_error": afterDiag ?? "",
        ])
        printJSON(["display_id": Int(displayID), "vcp": Int(vcp), "value": value,
                   "result": "ok", "previous": before ?? -1, "read_back": after ?? -1,
                   "note": "ddc_ok_is_machine_observed_only_picture_must_be_human_confirmed"])
        return 0
    } catch {
        let msg = String(describing: error)
        log.event("display.ddc.write", ["display_id": Int(displayID), "vcp": Int(vcp), "value": value,
                                        "result": "error", "error": msg])
        fputs("error: \(msg)\n", stderr)
        return 1
    }
}
