import Foundation

struct EdidInfo {
    let manufacturer: String      // EISA 3-letter code from bytes 8-9
    let productCode: Int          // bytes 10-11 LE (hex form is what vendors quote)
    let serialNumber: UInt32      // bytes 12-15 LE
    let week: Int                 // byte 16 (0 = unspecified)
    let year: Int                 // byte 17 + 1990
    let modelName: String?        // descriptor 0xFC
    let serialString: String?     // descriptor 0xFF
    let checksumOk: Bool
    let hex: String               // first 128 bytes, lowercase hex
    /// Stable identity for cross-run correlation: MANU-PPPP-SSSSSSSS (falls back to
    /// serial string when the numeric serial is 0 — common on cheap panels).
    let edidId: String
}

enum EdidError: Error, CustomStringConvertible {
    case badHeader
    case badChecksum
    var description: String {
        switch self {
        case .badHeader: return "edid_bad_header"
        case .badChecksum: return "edid_bad_checksum"
        }
    }
}

func parseEdid(_ e: [UInt8]) throws -> EdidInfo {
    guard e.count >= 128 else { throw EdidError.badHeader }
    guard e[0] == 0x00, e[1] == 0xFF, e[2] == 0xFF, e[3] == 0xFF,
          e[4] == 0xFF, e[5] == 0xFF, e[6] == 0xFF, e[7] == 0x00 else {
        throw EdidError.badHeader
    }
    let sum = e[0..<128].reduce(0) { $0 + Int($1) } & 0xFF
    let checksumOk = sum == 0

    let b0 = Int(e[8]), b1 = Int(e[9])
    let l1 = (b0 >> 2) & 0x1F
    let l2 = ((b0 & 0x03) << 3) | ((b1 >> 5) & 0x07)
    let l3 = b1 & 0x1F
    let manufacturer = String([l1, l2, l3].compactMap { v -> Character? in
        guard v > 0, v <= 26, let scalar = UnicodeScalar(64 + v) else { return nil }
        return Character(scalar)
    })

    let productCode = Int(e[10]) | (Int(e[11]) << 8)
    let serialNumber = UInt32(e[12]) | (UInt32(e[13]) << 8) | (UInt32(e[14]) << 16) | (UInt32(e[15]) << 24)

    var modelName: String?
    var serialString: String?
    for base in [54, 72, 90, 108] {
        guard e[base] == 0, e[base + 1] == 0 else { continue }
        let flag = e[base + 3]
        var text = ""
        var i = base + 5
        while i < base + 18, e[i] != 0x0A, e[i] != 0x00 {
            text.append(Character(UnicodeScalar(e[i])))
            i += 1
        }
        text = text.trimmingCharacters(in: .whitespacesAndNewlines)
        if flag == 0xFC, modelName == nil { modelName = text }
        if flag == 0xFF, serialString == nil { serialString = text }
    }

    var edidId: String
    if serialNumber != 0 {
        edidId = String(format: "%@-%04X-%08X", manufacturer, productCode, serialNumber)
    } else if let s = serialString, !s.isEmpty {
        edidId = String(format: "%@-%04X-S:%@", manufacturer, productCode, s)
    } else {
        edidId = String(format: "%@-%04X-NOSERIAL", manufacturer, productCode)
    }

    return EdidInfo(manufacturer: manufacturer, productCode: productCode, serialNumber: serialNumber,
                    week: Int(e[16]), year: Int(e[17]) + 1990,
                    modelName: modelName, serialString: serialString,
                    checksumOk: checksumOk,
                    hex: e[0..<128].map { String(format: "%02x", $0) }.joined(),
                    edidId: edidId)
}
