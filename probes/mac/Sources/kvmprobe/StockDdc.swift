import Foundation

/// Wrapper around the vendored, self-built stock m1ddc (v1.2.0, tag 2549fec)
/// - the confirmed MVP path for Mac DDC (2026-09-13 vendoring verdict: the
/// self-built binary reads true values on the target hardware where every
/// same-source reimplementation returned canned frames). kvmprobe keeps its
/// role as the single evidence logger: the child's argv, exit code and raw
/// stdout are recorded as display.ddc.stock_get / display.ddc.stock_set
/// JSONL events while stdout and the exit code pass through unchanged.
///
/// NOTE: `stock-ddc set` changes the monitor's actual input source - human
/// hardware session only, same rule as ddc-set.

enum StockDdcError: Error, CustomStringConvertible {
    case binaryNotFound([String])
    var description: String {
        switch self {
        case .binaryNotFound(let candidates):
            return "stock m1ddc binary not found (tried: \(candidates.joined(separator: ", "))) - build it first: cd probes/mac/experiments/m1ddc-stock && ./build.sh"
        }
    }
}

struct StockDdcResult {
    let binary: String
    let argv: [String]
    let code: Int32
    let stdout: String
    let stderrText: String
}

/// Search order for the self-built binary: $KVM_STOCK_M1DDC (authoritative
/// override - when set, no fallback), CWD-relative (session commands run from
/// probes/mac), then executable-relative (.build/release -> ../m1ddc-stock).
func stockBinaryCandidates() -> [String] {
    var candidates: [String] = []
    if let env = ProcessInfo.processInfo.environment["KVM_STOCK_M1DDC"], !env.isEmpty {
        candidates.append(env)
    }
    candidates.append(".build/m1ddc-stock/m1ddc-selfbuilt")
    if let exe = CommandLine.arguments.first {
        let exeDir = (exe as NSString).deletingLastPathComponent
        if !exeDir.isEmpty {
            candidates.append(exeDir + "/../m1ddc-stock/m1ddc-selfbuilt")
        }
    }
    return candidates
}

func resolveStockBinary() -> String? {
    // An explicit override is authoritative - when set, no fallback.
    if let env = ProcessInfo.processInfo.environment["KVM_STOCK_M1DDC"], !env.isEmpty {
        return FileManager.default.isExecutableFile(atPath: env) ? env : nil
    }
    return stockBinaryCandidates().first { FileManager.default.isExecutableFile(atPath: $0) }
}

/// Pure mapping from wrapper intent to the stock m1ddc child argv
/// (offline unit-testable): get -> display <N> get input;
/// set -> display <N> set input <V>.
func stockDdcChildArguments(subcommand: String, displayIndex: Int, value: Int?) -> [String]? {
    switch subcommand {
    case "get":
        guard value == nil else { return nil }
        return ["display", String(displayIndex), "get", "input"]
    case "set":
        guard let value = value else { return nil }
        return ["display", String(displayIndex), "set", "input", String(value)]
    default:
        return nil
    }
}

/// Spawn the stock binary and capture everything. `binaryOverride` exists for
/// offline selftest (e.g. /bin/echo); production calls pass nil and get the
/// standard resolution.
func runStockDdcCore(subcommand: String, displayIndex: Int, value: Int?,
                     binaryOverride: String? = nil) -> Result<StockDdcResult, StockDdcError> {
    guard let childArgs = stockDdcChildArguments(subcommand: subcommand, displayIndex: displayIndex, value: value) else {
        return .failure(.binaryNotFound(["<invalid subcommand \(subcommand)>"]))
    }
    let binary: String?
    if let override = binaryOverride {
        binary = FileManager.default.isExecutableFile(atPath: override) ? override : nil
    } else {
        binary = resolveStockBinary()
    }
    guard let bin = binary else {
        return .failure(.binaryNotFound(binaryOverride.map { [$0] } ?? stockBinaryCandidates()))
    }

    let proc = Process()
    proc.executableURL = URL(fileURLWithPath: bin)
    proc.arguments = childArgs
    let outPipe = Pipe()
    let errPipe = Pipe()
    proc.standardOutput = outPipe
    proc.standardError = errPipe
    do {
        try proc.run()
    } catch {
        return .failure(.binaryNotFound(["launch failed for \(bin): \(error)"]))
    }
    let outData = outPipe.fileHandleForReading.readDataToEndOfFile()
    let errData = errPipe.fileHandleForReading.readDataToEndOfFile()
    proc.waitUntilExit()
    return .success(StockDdcResult(
        binary: bin,
        argv: [bin] + childArgs,
        code: proc.terminationStatus,
        stdout: String(data: outData, encoding: .utf8) ?? "<non-utf8 \(outData.count) bytes>",
        stderrText: String(data: errData, encoding: .utf8) ?? ""
    ))
}

/// CLI entry: passthrough + JSONL evidence. Exit code: the child's own code,
/// or 3 when the stock binary is missing/unlaunchable. `binaryOverride` is
/// for offline selftest only.
func runStockDdc(subcommand: String, displayIndex: Int, value: Int?, log: SessionLog,
                 binaryOverride: String? = nil) -> Int32 {
    let event = subcommand == "set" ? "display.ddc.stock_set" : "display.ddc.stock_get"
    switch runStockDdcCore(subcommand: subcommand, displayIndex: displayIndex, value: value,
                           binaryOverride: binaryOverride) {
    case .failure(let e):
        var fields: [String: Any] = [
            "display_index": displayIndex,
            "result": "error",
            "error": "stock_binary_missing",
        ]
        if subcommand == "set" { fields["value"] = value ?? -1 }
        log.event(event, fields)
        fputs("error: \(e)\n", stderr)
        return 3
    case .success(let r):
        var fields: [String: Any] = [
            "display_index": displayIndex,
            "result": r.code == 0 ? "ok" : "error",
            "child_argv": r.argv,
            "child_exit_code": Int(r.code),
            "stdout": r.stdout,
            "stock_source": "m1ddc v1.2.0 tag 2549fec (vendored, self-built)",
        ]
        if subcommand == "set" {
            fields["value"] = value ?? -1
            fields["note"] = "ddc_ok_is_machine_observed_only_picture_must_be_human_confirmed"
        }
        if !r.stderrText.isEmpty { fields["stderr"] = r.stderrText }
        log.event(event, fields)
        // The child's stdout IS the evidence - pass it through verbatim.
        fputs(r.stdout, stdout)
        return r.code
    }
}
