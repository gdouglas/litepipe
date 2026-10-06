import XCTest
@testable import litepipe

/// The mic gate took "the reply parsed as JSON" to mean the engine did what it
/// asked. `/audio/start` and `/audio/stop` answer 200 with an empty body, so a
/// working gate was never recorded or logged, and an error that came back as a
/// JSON body would have been taken for success (#1). Success is the status.
final class EngineReplyTests: XCTestCase {

    func testAnEmpty200IsASuccess() {
        XCTAssertTrue(EngineReply(status: 200, data: Data()).succeeded)
    }

    func testAJsonErrorBodyIsAFailure() {
        let body = Data(#"{"success":false,"message":"Failed to start audio processing"}"#.utf8)
        XCTAssertFalse(EngineReply(status: 500, data: body).succeeded)
    }

    func testNoResponseIsAFailure() {
        XCTAssertFalse(EngineReply(status: nil, data: nil).succeeded)
    }

    func testTheJsonBodyIsStillAvailable() {
        let reply = EngineReply(status: 200, data: Data(#"{"active":true}"#.utf8))
        XCTAssertEqual((reply.json as? [String: Bool])?["active"], true)
    }
}

/// A stop can reach the engine before it has started audio at launch. The
/// engine answers 200 to a stop that did nothing, starts audio a few seconds
/// later, and the gate, now trusting its record, never sends stop again, so
/// the mic records outside meetings. The gate re-sends when the engine's own
/// health shows audio still arriving.
final class MicGateDriftTests: XCTestCase {

    private let closed = Date(timeIntervalSinceReferenceDate: 1_000_000)

    func testAudioStampedAfterTheGateClosedMeansStopAgain() {
        // The 2026-10-06 launch: stop at :46, the engine started audio at :52.
        XCTAssertTrue(MicGateDrift.engineStillRecording(
            gateInMeeting: false, closedAt: closed, lastAudio: closed + 6))
    }

    func testTheLastChunkOfALegitimateStopIsNotDrift() {
        XCTAssertFalse(MicGateDrift.engineStillRecording(
            gateInMeeting: false, closedAt: closed, lastAudio: closed + 2))
        XCTAssertFalse(MicGateDrift.engineStillRecording(
            gateInMeeting: false, closedAt: closed, lastAudio: closed - 20))
    }

    func testAnOpenOrUnknownGateIsLeftAlone() {
        XCTAssertFalse(MicGateDrift.engineStillRecording(
            gateInMeeting: true, closedAt: closed, lastAudio: closed + 60))
        XCTAssertFalse(MicGateDrift.engineStillRecording(
            gateInMeeting: nil, closedAt: nil, lastAudio: closed + 60))
    }

    func testTheEnginesTimestampFormatIsRead() {
        XCTAssertNotNil(MicGateDrift.parse("2026-10-06T00:18:17-07:00"))
        XCTAssertNotNil(MicGateDrift.parse("2026-10-06T07:18:17.123Z"))
        XCTAssertNil(MicGateDrift.parse(nil))
    }
}
