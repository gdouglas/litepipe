import XCTest
@testable import litepipe

/// Pausing from the window kills the engine, so the window refuses a second
/// toggle that arrives within a second of the last one. That guard used to
/// ignore direction, and refused the resume a person clicks straight after a
/// pause, leaving capture stopped (#2). It now refuses only a repeat.
final class CaptureToggleGuardTests: XCTestCase {

    private let t0 = Date(timeIntervalSinceReferenceDate: 1_000_000)

    func testTheFirstToggleIsAccepted() {
        var guardrail = CaptureToggleGuard()
        XCTAssertEqual(guardrail.decide(.pause, at: t0), .accepted)
    }

    func testAResumeRightAfterAPauseIsAccepted() {
        var guardrail = CaptureToggleGuard()
        _ = guardrail.decide(.pause, at: t0)
        XCTAssertEqual(guardrail.decide(.resume, at: t0 + 0.5), .accepted)
    }

    func testAPauseRightAfterAResumeIsAccepted() {
        var guardrail = CaptureToggleGuard()
        _ = guardrail.decide(.resume, at: t0)
        XCTAssertEqual(guardrail.decide(.pause, at: t0 + 0.5), .accepted)
    }

    func testARepeatedPauseInsideTheWindowIsRefused() {
        var guardrail = CaptureToggleGuard()
        _ = guardrail.decide(.pause, at: t0)
        XCTAssertEqual(guardrail.decide(.pause, at: t0 + 0.5), .refused(gap: 0.5))
    }

    func testARepeatedPauseAfterTheWindowIsAccepted() {
        var guardrail = CaptureToggleGuard()
        _ = guardrail.decide(.pause, at: t0)
        XCTAssertEqual(guardrail.decide(.pause, at: t0 + 1.5), .accepted)
    }

    func testARefusedRepeatDoesNotRestartTheWindow() {
        var guardrail = CaptureToggleGuard()
        _ = guardrail.decide(.pause, at: t0)
        _ = guardrail.decide(.pause, at: t0 + 0.8)
        XCTAssertEqual(guardrail.decide(.pause, at: t0 + 1.2), .accepted)
    }
}
