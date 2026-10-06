import Foundation

/// Decides whether a pause or resume from the window goes through.
///
/// Pausing kills the engine process, so a stray toggle stops recording. The
/// log shows toggles arriving in pairs seconds apart, which no person clicking
/// a menu produces, so a second toggle inside the window is refused until the
/// source is found. Only a repeat of the same direction is refused: a reversal
/// moves capture to the state it is not in, which is always safe, and refusing
/// the resume that follows a pause left capture stopped with nothing to say so.
struct CaptureToggleGuard {
    enum Direction: String {
        case pause
        case resume
    }

    enum Decision: Equatable {
        case accepted
        case refused(gap: TimeInterval)
    }

    static let window: TimeInterval = 1.0

    private var last: (direction: Direction, at: Date)?

    mutating func decide(_ direction: Direction, at now: Date = Date()) -> Decision {
        if let last, last.direction == direction {
            let gap = now.timeIntervalSince(last.at)
            if gap <= Self.window {
                return .refused(gap: gap)
            }
        }
        last = (direction, now)
        return .accepted
    }
}
