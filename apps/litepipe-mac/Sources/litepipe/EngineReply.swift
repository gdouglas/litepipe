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

/// Whether the engine is still recording audio after the gate closed it.
///
/// A stop can arrive before the engine has started audio at launch: the engine
/// answers 200 to a stop that did nothing, starts audio seconds later, and the
/// gate, trusting its record, would never send stop again. The engine's own
/// `last_audio_timestamp` is the check. Audio stamped after the gate closed,
/// past a grace for the final chunk of a real stop, means stop again.
enum MicGateDrift {
    static let grace: TimeInterval = 5

    static func engineStillRecording(gateInMeeting: Bool?, closedAt: Date?, lastAudio: Date?) -> Bool {
        guard gateInMeeting == false, let closedAt, let lastAudio else { return false }
        return lastAudio > closedAt + grace
    }

    static func parse(_ raw: String?) -> Date? {
        guard let raw else { return nil }
        let plain = ISO8601DateFormatter()
        if let d = plain.date(from: raw) { return d }
        let fractional = ISO8601DateFormatter()
        fractional.formatOptions = [.withInternetDateTime, .withFractionalSeconds]
        return fractional.date(from: raw)
    }
}
