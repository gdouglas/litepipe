import Foundation

/// One answer from the engine's local API. Whether a call worked is the HTTP
/// status, not whether the body parses: several endpoints answer an empty 200,
/// and an error can come back with a JSON body.
struct EngineReply {
    let status: Int?
    let data: Data?

    var succeeded: Bool {
        guard let status else { return false }
        return (200..<300).contains(status)
    }

    var json: Any? {
        data.flatMap { try? JSONSerialization.jsonObject(with: $0) }
    }
}
