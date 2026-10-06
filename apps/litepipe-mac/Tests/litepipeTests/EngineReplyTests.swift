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
