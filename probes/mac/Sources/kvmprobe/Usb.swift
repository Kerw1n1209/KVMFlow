import Foundation
import IOKit
import IOKit.usb

let usbDeviceClasses = ["IOUSBDevice", "IOUSBHostDevice"]

private func strProp(_ service: io_service_t, _ name: String) -> String? {
    guard let cf = IORegistryEntryCreateCFProperty(service, name as CFString, kCFAllocatorDefault, 0)?.takeRetainedValue(),
          let s = cf as? String else { return nil }
    return s
}

private func numProp(_ service: io_service_t, _ name: String) -> Int? {
    guard let cf = IORegistryEntryCreateCFProperty(service, name as CFString, kCFAllocatorDefault, 0)?.takeRetainedValue() else { return nil }
    if let n = cf as? NSNumber { return n.intValue }
    if let s = cf as? String { return Int(s) }
    return nil
}

func usbDeviceFields(_ service: io_service_t) -> [String: Any] {
    let vid = numProp(service, "idVendor") ?? -1
    let pid = numProp(service, "idProduct") ?? -1
    let serial = strProp(service, "USB Serial Number") ?? ""
    let vendor = strProp(service, "USB Vendor Name") ?? ""
    let product = strProp(service, "USB Product Name") ?? ""
    var rid: UInt64 = 0
    IORegistryEntryGetRegistryEntryID(service, &rid)
    var fields: [String: Any] = [
        "key": String(format: "%04x:%04x:%@", vid, pid, serial),
        "vid": vid,
        "pid": pid,
        "vid_pid": String(format: "%04x:%04x", vid, pid),
        "vendor": vendor,
        "product": product,
        "serial": serial,
        "registry_id": Int(rid),
    ]
    if let loc = numProp(service, "locationID") { fields["location_id"] = loc }
    return fields
}

/// Enumerate all USB devices visible to IOKit, deduped by registry entry ID.
func usbSnapshot() -> [[String: Any]] {
    var seen = Set<UInt64>()
    var devices: [[String: Any]] = []
    for cls in usbDeviceClasses {
        guard let matching = IOServiceMatching(cls) else { continue }
        var iterator: io_iterator_t = 0
        guard IOServiceGetMatchingServices(kIOMainPortDefault, matching, &iterator) == KERN_SUCCESS else { continue }
        while true {
            let service = IOIteratorNext(iterator)
            guard service != 0 else { break }
            defer { IOObjectRelease(service) }
            var rid: UInt64 = 0
            IORegistryEntryGetRegistryEntryID(service, &rid)
            guard !seen.contains(rid) else { continue }
            seen.insert(rid)
            devices.append(usbDeviceFields(service))
        }
        IOObjectRelease(iterator)
    }
    devices.sort { a, b in
        let ka = a["key"] as? String ?? ""
        let kb = b["key"] as? String ?? ""
        return ka < kb
    }
    return devices
}

func runUsbSnapshot(outFile: String?, log: SessionLog) -> Int32 {
    let devices = usbSnapshot()
    log.event("usb.snapshot", ["count": devices.count])
    for d in devices {
        log.event("usb.device", d)
    }
    let payload: [String: Any] = ["count": devices.count, "devices": devices]
    if let out = outFile {
        let url = URL(fileURLWithPath: (out as NSString).expandingTildeInPath)
        if let data = try? JSONSerialization.data(withJSONObject: payload, options: [.prettyPrinted, .sortedKeys]) {
            try? data.write(to: url)
            fputs("snapshot written: \(url.path) (\(devices.count) devices)\n", stderr)
        }
    }
    printJSON(payload)
    return 0
}

func runUsbDiff(beforePath: String, afterPath: String, log: SessionLog) -> Int32 {
    func load(_ p: String) -> [String: [String: Any]] {
        let url = URL(fileURLWithPath: (p as NSString).expandingTildeInPath)
        guard let data = try? Data(contentsOf: url),
              let obj = try? JSONSerialization.jsonObject(with: data) as? [String: Any],
              let devices = obj["devices"] as? [[String: Any]] else {
            fputs("error: cannot read snapshot \(p)\n", stderr)
            return [:]
        }
        var map: [String: [String: Any]] = [:]
        for d in devices { if let k = d["key"] as? String { map[k] = d } }
        return map
    }
    let before = load(beforePath)
    let after = load(afterPath)
    let removed = before.keys.filter { after[$0] == nil }.sorted().map { before[$0]! }
    let added = after.keys.filter { before[$0] == nil }.sorted().map { after[$0]! }
    let unchanged = before.keys.filter { after[$0] != nil }.count
    log.event("usb.diff", [
        "before": beforePath, "after": afterPath,
        "added": added.count, "removed": removed.count, "unchanged": unchanged,
    ])
    printJSON([
        "before": beforePath, "after": afterPath,
        "added_count": added.count, "removed_count": removed.count, "unchanged_count": unchanged,
        "added": added, "removed": removed,
        "note": "trigger-device candidates appear as removed-on-this-host when the switch moves away",
    ])
    return 0
}

/// Continuous USB connect/disconnect monitor via IOKit matching notifications.
final class UsbWatch {
    let log: SessionLog
    private var tickSource: DispatchSourceTimer?

    init(log: SessionLog) {
        self.log = log
    }

    private func drain(_ iterator: io_iterator_t, eventName: String, silent: Bool) {
        while true {
            let service = IOIteratorNext(iterator)
            guard service != 0 else { break }
            defer { IOObjectRelease(service) }
            guard !silent else { continue }
            log.event("usb.\(eventName)", usbDeviceFields(service), echo: true)
        }
    }

    func run(duration: Double?) {
        let port = IONotificationPortCreate(kIOMainPortDefault)
        let runLoopSource = IONotificationPortGetRunLoopSource(port).takeUnretainedValue()
        CFRunLoopAddSource(CFRunLoopGetCurrent(), runLoopSource, .defaultMode)

        let ctx = Unmanaged.passRetained(self).toOpaque()

        // The first iterator delivered right after registration contains every
        // already-present device; drain it silently so only post-start events are logged.
        for cls in usbDeviceClasses {
            guard let matching = IOServiceMatching(cls) else { continue }
            var matchedIter: io_iterator_t = 0
            let matchedCb: IOServiceMatchingCallback = { ctx, iterator in
                let watch = Unmanaged<UsbWatch>.fromOpaque(ctx!).takeUnretainedValue()
                watch.drain(iterator, eventName: "connect", silent: false)
            }
            IOServiceAddMatchingNotification(port, kIOMatchedNotification, matching, matchedCb, ctx, &matchedIter)
            drain(matchedIter, eventName: "connect", silent: true)

            var termIter: io_iterator_t = 0
            let termCb: IOServiceMatchingCallback = { ctx, iterator in
                let watch = Unmanaged<UsbWatch>.fromOpaque(ctx!).takeUnretainedValue()
                watch.drain(iterator, eventName: "disconnect", silent: false)
            }
            IOServiceAddMatchingNotification(port, kIOTerminatedNotification, IOServiceMatching(cls), termCb, ctx, &termIter)
            drain(termIter, eventName: "disconnect", silent: true)
        }

        let initialCount = usbSnapshot().count
        log.event("usb.watch.start", ["known_devices": initialCount, "duration_sec": duration ?? -1], echo: true)

        // 10s heartbeat so a silently-dead watcher is distinguishable from a quiet bus.
        let logger = log
        let tick = DispatchSource.makeTimerSource(queue: .main)
        tick.schedule(deadline: .now() + 10, repeating: 10)
        tick.setEventHandler { logger.event("usb.watch.tick", [:], echo: true) }
        tick.resume()
        tickSource = tick

        signal(SIGINT, SIG_IGN)
        let sigint = DispatchSource.makeSignalSource(signal: SIGINT, queue: .main)
        sigint.setEventHandler { CFRunLoopStop(CFRunLoopGetCurrent()) }
        sigint.resume()

        if let d = duration, d > 0 {
            DispatchQueue.main.asyncAfter(deadline: .now() + d) { CFRunLoopStop(CFRunLoopGetCurrent()) }
        }
        CFRunLoopRun()

        log.event("usb.watch.end", [:], echo: true)
        exit(0)
    }
}
