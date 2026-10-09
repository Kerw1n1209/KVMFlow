import Foundation
import m1ddcShim

/// Built-in unit checks for the pure parsing paths, runnable on any machine:
/// `kvmprobe selftest`. Fixtures include the byte patterns observed from the two
/// real external monitors during the 2026-09-13 baseline session, including the
/// m1ddc 12-byte reply layout adopted in 0.1.4.
func runSelftest() -> Int32 {
    var failures = 0
    var passes = 0
    func check(_ name: String, _ cond: Bool) {
        if cond { passes += 1; print("  PASS  \(name)") }
        else { failures += 1; print("  FAIL  \(name)") }
    }
    func hexBytes(_ hex: String) -> [UInt8] {
        var out: [UInt8] = []
        var i = hex.startIndex
        while i < hex.endIndex {
            let j = hex.index(i, offsetBy: 2)
            out.append(UInt8(hex[i..<j], radix: 16)!); i = j
        }
        return out
    }
    func hexStr(_ b: [UInt8]) -> String {
        b.map { String(format: "%02x", $0) }.joined(separator: " ")
    }

    print("== VCP reply parser ==")
    // valid reply: code 0x60, max 0x1b, current 0x0f, correct checksum
    var valid = [UInt8]([0x6E, 0x8E, 0x60, 0x00, 0x1B, 0x00, 0x0F])
    valid.append(ddcChecksum(valid[0..<7]))
    if case .success(let r) = parseVcpReply(valid, code: 0x60) {
        check("valid reply parses", r.current == 0x0F && r.max == 0x1B && r.code == 0x60)
    } else { check("valid reply parses", false) }

    // m1ddc 12-byte layout: 3 pad bytes, frame at offset 3 (cur at [8..9])
    var frame = [UInt8]([0x6E, 0x8E, 0x60, 0x00, 0x1B, 0x00, 0x0F])
    frame.append(ddcChecksum(frame[0..<7]))
    let m1Layout = [UInt8](repeating: 0, count: 3) + frame + [UInt8](repeating: 0, count: 12 - 3 - 8)
    if case .success(let r) = parseVcpReply(m1Layout, code: 0x60) {
        check("m1ddc 12-byte layout (frame at offset 3) parses, current=\(r.current)",
              r.current == 0x0F && r.max == 0x1B)
    } else { check("m1ddc 12-byte layout parses", false) }

    // EDID read immediately before VCP read: valid frame with stale ASCII tail
    let staleTail = frame + [0x30, 0x30, 0x30, 0x30]
    if case .success(let r) = parseVcpReply(staleTail, code: 0x60) {
        check("valid frame with stale EDID-residue tail still parses", r.current == 0x0F)
    } else { check("valid frame with stale tail parses", false) }

    // real bytes from monitor G73 (SAC-2763): DDC/CI null message + stale tail
    let g73Bytes: [UInt8] = [0x6E, 0x80, 0xBE, 0x30, 0x30, 0x30, 0x30, 0x30]
    if case .failure(let e) = parseVcpReply(g73Bytes, code: 0x60) {
        check("G73 null message classified distinctly (not bad_header, not crash)",
              String(describing: e).contains("ddc_null_message"))
    } else { check("G73 null message classified distinctly", false) }

    // real 12-byte frame recorded at the 0xB7 fallback address on BOTH monitors
    // (byte-identical, contains neither 15/7 nor 16/8, fails every checksum
    // convention) - a canned/bridge response, not per-monitor VCP data. The
    // parser must accept it as a frame-shaped reply but refuse to decode it.
    let altAddrFrame: [UInt8] = [0x6E, 0x88, 0x02, 0x01, 0x3C, 0x01, 0xFF, 0xFF, 0x00, 0x00, 0x88, 0x30]
    if case .failure(let e) = parseVcpReply(altAddrFrame, code: 0x60) {
        check("0xB7 canned frame rejected by checksum gate (not ok, not null)",
              String(describing: e).contains("reply_checksum_mismatch"))
    } else { check("0xB7 canned frame rejected", false) }

    // FORENSIC FIXTURES (0.1.8 port era): the two frames the 0x37 single-address
    // probe runs returned, per monitor, on real hardware (2026-09-13 09:42 run).
    // They differ ONLY in the tail byte (G73: 0x30, G52plus: 0x49) - the first
    // byte-level divergence ever observed between the two monitors in this
    // reply family. Under m1ddc's blind offsets both decode to cur=0/max=0xFFFF,
    // NOT to the 15/7 m1ddc itself printed on the same machine - so these are
    // not the frames a successful m1ddc exchange sees. The m1ddc shim port
    // (0.1.8) exists precisely to make the probe's exchange identical to
    // m1ddc's; these fixtures preserve the pre-port evidence.
    let g73_017_frame: [UInt8] = [0x6E, 0x88, 0x02, 0x01, 0x3C, 0x01, 0xFF, 0xFF, 0x00, 0x00, 0x88, 0x30]
    let g52plus_017_frame: [UInt8] = [0x6E, 0x88, 0x02, 0x01, 0x3C, 0x01, 0xFF, 0xFF, 0x00, 0x00, 0x88, 0x49]
    if case .failure(let e1) = parseVcpReply(g73_017_frame, code: 0x60),
       case .failure(let e2) = parseVcpReply(g52plus_017_frame, code: 0x60) {
        check("pre-port 0x37 frames (G73 …88 30 / G52plus …88 49) stay checksum-rejected",
              String(describing: e1).contains("reply_checksum_mismatch") &&
              String(describing: e2).contains("reply_checksum_mismatch"))
    } else { check("pre-port frames stay rejected", false) }

    // dialect with helper bytes and a non-0x8E length byte:
    // [6E][88][02][01][code][mh][ml][sh][sl][chk] - parses when checksum is valid
    var ext = [UInt8]([0x6E, 0x88, 0x02, 0x01, 0x60, 0x00, 0x1B, 0x00, 0x0F])
    ext.append(ddcChecksum(ext[0..<9]))
    let extBuf = ext + [0x30, 0x30]
    if case .success(let r) = parseVcpReply(extBuf, code: 0x60) {
        check("extended dialect [6E 88 02 01 code ...] parses with valid checksum",
              r.current == 0x0F && r.max == 0x1B)
    } else { check("extended dialect parses", false) }

    // valid header, corrupted checksum
    var corrupt = valid; corrupt[7] ^= 0xFF
    if case .failure(let e) = parseVcpReply(corrupt, code: 0x60) {
        check("checksum mismatch detected", String(describing: e).contains("reply_checksum_mismatch"))
    } else { check("checksum mismatch detected", false) }

    // short buffer
    if case .failure(let e) = parseVcpReply([0x6E, 0x8E], code: 0x60) {
        check("short reply rejected", String(describing: e).contains("reply_too_short"))
    } else { check("short reply rejected", false) }

    print("== EDID parser ==")
    let sac2763Hex = "00ffffffffffff004c236327000000000522010400000000000000000000000000000000000000000000000000000000000000000000000000fc0054455354204737330000000000000000ff00303030303030303030303030310000000000000000000000000000000000000000000000000000000000000000000000000064"
    if let info = try? parseEdid(hexBytes(sac2763Hex)) {
        check("SAC2763 fixture -> SAC-2763-S:0000000000001 (matches real monitor)",
              info.edidId == "SAC-2763-S:0000000000001" && info.checksumOk)
    } else { check("SAC2763 fixture parses", false) }

    let sac2466Hex = "00ffffffffffff004c236624000000000522010400000000000000000000000000000000000000000000000000000000000000000000000000fc005445535420473532706c757300000000ff003030303030303030303030303000000000000000000000000000000000000000000000000000000000000000000000000000a4"
    if let info = try? parseEdid(hexBytes(sac2466Hex)) {
        check("SAC2466 fixture -> SAC-2466-S:0000000000000 (matches real monitor)",
              info.edidId == "SAC-2466-S:0000000000000" && info.checksumOk)
    } else { check("SAC2466 fixture parses", false) }

    print("== command construction (m1ddc-replicated) ==")
    // Get VCP request must be [0x82, 0x01, code, chk] with 0x6E seed
    var getReq: [UInt8] = [0x82, 0x01, 0x60, 0]
    getReq[3] = 0x6E ^ getReq[0] ^ getReq[1] ^ getReq[2]
    check("get request bytes/chk match m1ddc ([82 01 60 chk], seed 0x6E)",
          hexStr(getReq) == "82 01 60 " + String(format: "%02x", 0x6E ^ 0x82 ^ 0x01 ^ 0x60))
    // Set VCP request: [0x84, 0x03, code, hi, lo, chk] seed 0x6E+0x51
    var setReq: [UInt8] = [0x84, 0x03, 0x60, 0x00, 0x0F, 0]
    setReq[5] = 0x6E ^ UInt8(ddcCiSubAddress) ^ setReq[0] ^ setReq[1] ^ setReq[2] ^ setReq[3] ^ setReq[4]
    let expectChk = 0x6E ^ 0x51 ^ 0x84 ^ 0x03 ^ 0x60 ^ 0x00 ^ 0x0F
    check("set request bytes/chk match m1ddc ([84 03 60 00 0f chk])",
          hexStr(setReq) == String(format: "84 03 60 00 0f %02x", expectChk))

    print("== m1ddc 1.2.0 discovery walk (pure core) ==")
    // Locks the selection semantics of m1ddc v1.2.0 (tag 2549fec)
    // getDisplayAVService: the entry whose IOService path equals the
    // display's IODisplayLocation arms the scan, the SAME iterator keeps
    // walking (no sibling scoping, no re-arming), and the first
    // DCPAVServiceProxy with Location "External" wins. 0.1.10 replaced the
    // master-algorithm discovery with this walk - the brew-installed 1.2.0
    // reference reads correct values on the target hardware.
    func walkSelect(_ entries: [(path: String?, name: String?, location: String?)],
                    ioLocation: String?) -> Int {
        var walk = KvmV120Walk()
        _ = kvm_v120_walk_init(&walk, ioLocation)
        for (i, e) in entries.enumerated() {
            var select: Int32 = 0
            kvm_v120_walk_step(&walk, e.path, e.name, e.location, &select)
            if select != 0 { return i }
        }
        return -1
    }
    let loc = "IOService:/AppleARMPE/display@0"
    // Internal proxy skipped, non-proxy skipped, External proxy under a
    // DIFFERENT display's subtree still selected (no sibling scoping)
    let e1: [(path: String?, name: String?, location: String?)] = [
        ("IOService:/other/hid", "AppleUserHIDEventDriver", nil),
        (loc, "AppleCLCD2", nil),
        ("IOService:/dcpext0/proxy", "DCPAVServiceProxy", "Internal"),
        ("IOService:/dcpext0/fb", "IOMobileFramebuffer", nil),
        ("IOService:/dcpext1/proxy", "DCPAVServiceProxy", "External"),
    ]
    check("1.2.0 walk: first External proxy after display node wins (no sibling scoping)",
          walkSelect(e1, ioLocation: loc) == 4)
    let e2: [(path: String?, name: String?, location: String?)] = [
        ("IOService:/dcpext1/proxy", "DCPAVServiceProxy", "External"),
        (loc, "AppleCLCD2", nil),
        ("IOService:/dcpext1/proxy2", "DCPAVServiceProxy", "External"),
    ]
    check("1.2.0 walk: proxy before the display node is ignored",
          walkSelect(e2, ioLocation: loc) == 2)
    let e3: [(path: String?, name: String?, location: String?)] = [
        (loc, "AppleCLCD2", nil),
        (loc, "AppleCLCD2-copy", nil),   // second path match: ordinary entry
        ("IOService:/x", "DCPAVServiceProxy", "External"),
    ]
    check("1.2.0 walk: second path-equal entry does not re-arm",
          walkSelect(e3, ioLocation: loc) == 2)
    let e4: [(path: String?, name: String?, location: String?)] = [
        (loc, "AppleCLCD2", nil),
        ("IOService:/x", "DCPAVServiceProxy", nil),   // Location property absent
        ("IOService:/y", "DCPAVServiceProxy", "External"),
    ]
    check("1.2.0 walk: proxy without Location property skipped",
          walkSelect(e4, ioLocation: loc) == 2)
    let e5: [(path: String?, name: String?, location: String?)] = [
        (loc, "AppleCLCD2", nil),
        ("IOService:/x", "DCPAVServiceProxy", "Internal"),
        ("IOService:/y", "IOMobileFramebuffer", nil),
    ]
    check("1.2.0 walk: armed with no External proxy selects nothing",
          walkSelect(e5, ioLocation: loc) == -1)
    check("1.2.0 walk: NULL IODisplayLocation never arms",
          walkSelect(e5, ioLocation: nil) == -1)

    print("== stock-ddc wrapper (offline) ==")
    // argv mapping (pure)
    check("stock-ddc argv get -> display N get input",
          stockDdcChildArguments(subcommand: "get", displayIndex: 3, value: nil) == ["display", "3", "get", "input"])
    check("stock-ddc argv set -> display N set input V",
          stockDdcChildArguments(subcommand: "set", displayIndex: 1, value: 16) == ["display", "1", "set", "input", "16"])
    check("stock-ddc argv rejects set without value / unknown subcommand",
          stockDdcChildArguments(subcommand: "set", displayIndex: 1, value: nil) == nil &&
          stockDdcChildArguments(subcommand: "list", displayIndex: 1, value: nil) == nil)
    // spawn + passthrough via a stand-in binary (no stock binary needed)
    if case .success(let r) = runStockDdcCore(subcommand: "get", displayIndex: 3, value: nil,
                                              binaryOverride: "/bin/echo") {
        check("stock-ddc core spawns child, captures stdout, exit 0",
              r.code == 0 && r.stdout.contains("display 3 get input"))
    } else { check("stock-ddc core spawns child", false) }
    if case .success(let r) = runStockDdcCore(subcommand: "get", displayIndex: 3, value: nil,
                                              binaryOverride: "/usr/bin/false") {
        check("stock-ddc core passes child exit code through (1)",
              r.code == 1)
    } else { check("stock-ddc core passes exit code through", false) }
    if case .failure(let e) = runStockDdcCore(subcommand: "get", displayIndex: 3, value: nil,
                                              binaryOverride: "/nonexistent/kvm-selftest") {
        var isMissingCase = false
        if case .binaryNotFound = e { isMissingCase = true }
        check("stock-ddc core reports missing binary with candidates", isMissingCase)
    } else { check("stock-ddc core reports missing binary", false) }
    // the CLI wrapper maps a missing binary to exit 3 (deterministic via override)
    let nilLog = SessionLog(path: nil)
    let rc3 = runStockDdc(subcommand: "get", displayIndex: 3, value: nil, log: nilLog,
                          binaryOverride: "/nonexistent/kvm-selftest")
    check("stock-ddc CLI returns 3 when the binary is missing", rc3 == 3)

    print("")
    print("result: \(passes) passed, \(failures) failed")
    return failures > 0 ? 1 : 0
}
