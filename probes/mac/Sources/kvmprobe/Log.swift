import Foundation

/// Append-only JSONL session log shared by all probe commands.
/// One JSON object per line; schema documented in probes/README.md.
final class SessionLog {
    static let tool = "kvmprobe-mac"
    static let version = "0.1.12"

    let logPath: URL?
    private let lock = NSLock()
    private let fmt: ISO8601DateFormatter

    init(path: String?) {
        self.logPath = path.map { URL(fileURLWithPath: ($0 as NSString).expandingTildeInPath) }
        let f = ISO8601DateFormatter()
        f.formatOptions = [.withInternetDateTime, .withFractionalSeconds]
        self.fmt = f
    }

    static func timestamp(_ date: Date = Date()) -> String {
        let f = ISO8601DateFormatter()
        f.formatOptions = [.withInternetDateTime, .withFractionalSeconds]
        return f.string(from: date)
    }

    /// Emit one event. `echo` also prints the JSONL line to stdout (used by watch mode).
    func event(_ name: String, _ fields: [String: Any] = [:], echo: Bool = false) {
        var payload: [String: Any] = [
            "ts": SessionLog.timestamp(),
            "tool": SessionLog.tool,
            "version": SessionLog.version,
            "event": name,
        ]
        for (k, v) in fields { payload[k] = jsonSafe(v) }

        let line: String
        if JSONSerialization.isValidJSONObject(payload),
           let data = try? JSONSerialization.data(withJSONObject: payload, options: [.sortedKeys]),
           let s = String(data: data, encoding: .utf8) {
            line = s
        } else {
            // last-resort single-line fallback so a bad value never loses the event
            let parts = payload.map { (k, v) in "\(k)=\(String(describing: v))" }.sorted().joined(separator: " ")
            line = "{\"ts\":\"\(SessionLog.timestamp())\",\"event\":\"\(name)\",\"raw\":\"\(parts.replacingOccurrences(of: "\"", with: "'"))\"}"
        }
        let jsonl = line + "\n"

        lock.lock()
        if echo {
            fputs(jsonl, stdout)
            fflush(stdout)
        }
        if let url = logPath {
            try? FileManager.default.createDirectory(at: url.deletingLastPathComponent(),
                                                     withIntermediateDirectories: true)
            if !FileManager.default.fileExists(atPath: url.path) {
                FileManager.default.createFile(atPath: url.path, contents: nil)
            }
            if let h = FileHandle(forWritingAtPath: url.path) {
                defer { h.closeFile() }
                h.seekToEndOfFile()
                if let d = jsonl.data(using: .utf8) { h.write(d) }
            }
        }
        lock.unlock()
    }

    private func jsonSafe(_ v: Any) -> Any {
        switch v {
        case let s as String: return s
        case let n as NSNumber: return n
        case let b as Bool: return b
        case let a as [Any]: return a.map { jsonSafe($0) }
        case let d as [String: Any]:
            var out: [String: Any] = [:]
            for (k, vv) in d { out[k] = jsonSafe(vv) }
            return out
        case is NSNull: return NSNull()
        default: return String(describing: v)
        }
    }
}

/// Pretty-print JSON to stdout.
func printJSON(_ value: Any) {
    if JSONSerialization.isValidJSONObject(value),
       let data = try? JSONSerialization.data(withJSONObject: value, options: [.prettyPrinted, .sortedKeys]),
       let s = String(data: data, encoding: .utf8) {
        print(s)
    } else {
        print(String(describing: value))
    }
}
