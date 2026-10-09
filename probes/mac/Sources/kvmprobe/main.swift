import Foundation

func usage() -> Never {
    let u = """
    kvmprobe — KVMFlow hardware verification probe (macOS side)
    Common flags: --log <file>   append JSONL session events to <file>

    Commands:
      display-probe  [--log F]              enumerate displays, read EDID, test VCP 60 read
      ddc-get <displayID> [--vcp N] [--log F]   read one VCP value (default 60)
      ddc-raw <displayID> [--vcp N] [--chip 0x37] [--lean] [--m1ddc-prelude] [--prelude-attrs] [--log F]
                                            shim-only discovery + one 12-byte raw read;
                                            --lean = zero IOKit calls between proxy selection
                                            and the exchange (evidence filled after);
                                            --m1ddc-prelude = replicate m1ddc's pre-DDC call
                                            chain first; --prelude-attrs adds its property reads
      ddc-set <displayID> <value> [--vcp N] [--log F]
                                            WRITE one VCP value — changes monitor input,
                                            only for the human hardware session
      stock-ddc get <displayIndex> [--log F]
                                            run the vendored self-built stock m1ddc
                                            (display <N> get input), stdout/exit passthrough,
                                            JSONL evidence event display.ddc.stock_get
      stock-ddc set <displayIndex> <value> [--log F]
                                            same via display <N> set input <V> — CHANGES
                                            monitor input, human hardware session only
      usb-snapshot   [-o F] [--log F]       JSON snapshot of all USB devices
      usb-diff <before.json> <after.json>   compare two snapshots
      usb-watch      [--log F] [--duration SEC]  stream usb.connect/usb.disconnect JSONL
                                            (default runs until Ctrl-C)
      selftest                              run built-in parser unit checks (no hardware)
      version
    """
    print(u)
    exit(64)
}

func fail(_ msg: String) -> Never {
    fputs("error: \(msg)\n", stderr)
    exit(64)
}

var args = Array(CommandLine.arguments.dropFirst())
guard let cmd = args.first else { usage() }
args = Array(args.dropFirst())

func takeValue(flag: String) -> String? {
    guard let i = args.firstIndex(of: flag) else { return nil }
    guard i + 1 < args.count else { fail("missing value for \(flag)") }
    let v = args[i + 1]
    args.removeSubrange(i...i + 1)
    return v
}

let logPath = takeValue(flag: "--log")
let log = SessionLog(path: logPath)
log.event("session.start", ["argv": CommandLine.arguments, "host": Host.current().localizedName ?? "mac"])

defer { log.event("session.end", [:]) }

switch cmd {
case "version":
    print("kvmprobe \(SessionLog.version)")

case "selftest":
    exit(runSelftest())

case "display-probe":
    let reports = displayProbe(log: log)
    printJSON(["displays": reports.map { $0.json }])
    if reports.allSatisfy({ ($0.json["builtin"] as? Bool) == true }) {
        fputs("# note: no external display attached — DDC paths untested until hardware session\n", stderr)
    }

case "ddc-get":
    guard let idStr = args.first, let id = UInt32(idStr) else { fail("usage: ddc-get <displayID> [--vcp N] [--chip 0x37]") }
    let vcp = UInt8(takeValue(flag: "--vcp").flatMap(Int.init) ?? 60)
    let chipOverride = takeValue(flag: "--chip").flatMap { UInt32($0.dropFirst($0.hasPrefix("0x") ? 2 : 0), radix: 16) }
    let rc = runDdcGet(displayID: id, vcp: vcp, chipOverride: chipOverride, log: log)
    log.event("session.end", [:])   // exit() below skips the top-level defer
    exit(rc)

case "ddc-raw":
    guard let idStr = args.first, let id = UInt32(idStr) else {
        fail("usage: ddc-raw <displayID> [--vcp N] [--chip 0x37] [--lean] [--m1ddc-prelude] [--prelude-attrs]")
    }
    let vcp = UInt8(takeValue(flag: "--vcp").flatMap(Int.init) ?? 60)
    let chipOverride = takeValue(flag: "--chip").flatMap {
        UInt32($0.dropFirst($0.hasPrefix("0x") ? 2 : 0), radix: 16)
    }
    let lean = args.contains("--lean")
    let prelude = args.contains("--m1ddc-prelude")
    let preludeAttrs = args.contains("--prelude-attrs")
    let rc = runDdcRaw(displayID: id, vcp: vcp, chipOverride: chipOverride, lean: lean,
                       prelude: prelude, preludeAttrs: preludeAttrs, log: log)
    log.event("session.end", [:])
    exit(rc)

case "ddc-set":
    guard args.count >= 2, let id = UInt32(args[0]), let value = Int(args[1]) else {
        fail("usage: ddc-set <displayID> <value> [--vcp N]")
    }
    let vcp = UInt8(takeValue(flag: "--vcp").flatMap(Int.init) ?? 60)
    let rc = runDdcSet(displayID: id, vcp: vcp, value: value, log: log)
    log.event("session.end", [:])
    exit(rc)

case "stock-ddc":
    // stock-ddc get <displayIndex> | stock-ddc set <displayIndex> <value>
    guard args.count >= 2 else {
        fail("usage: stock-ddc get <displayIndex> | stock-ddc set <displayIndex> <value>")
    }
    let rc: Int32
    switch args[0] {
    case "get":
        guard let n = Int(args[1]), n >= 1 else { fail("usage: stock-ddc get <displayIndex>") }
        rc = runStockDdc(subcommand: "get", displayIndex: n, value: nil, log: log)
    case "set":
        guard args.count >= 3, let n = Int(args[1]), n >= 1, let v = Int(args[2]) else {
            fail("usage: stock-ddc set <displayIndex> <value>")
        }
        rc = runStockDdc(subcommand: "set", displayIndex: n, value: v, log: log)
    default:
        fail("usage: stock-ddc get <displayIndex> | stock-ddc set <displayIndex> <value>")
    }
    log.event("session.end", [:])
    exit(rc)

case "usb-snapshot":
    let out = takeValue(flag: "-o")
    let rc = runUsbSnapshot(outFile: out, log: log)
    log.event("session.end", [:])
    exit(rc)

case "usb-diff":
    guard args.count >= 2 else { fail("usage: usb-diff <before.json> <after.json>") }
    let rc = runUsbDiff(beforePath: args[0], afterPath: args[1], log: log)
    log.event("session.end", [:])   // exit() below skips the top-level defer
    exit(rc)

case "usb-watch":
    let duration = takeValue(flag: "--duration").flatMap(Double.init)
    log.event("session.end", [:])   // watch exits via exit(0); run defer early
    UsbWatch(log: log).run(duration: duration)

default:
    usage()
}
