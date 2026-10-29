// Checks output produced by the Rust client against the bitchat iOS code.
// Reads $BITCHAT_INTEROP_DIR/rust/*.json; each test is skipped when its file is missing.

import Foundation
import Testing
@testable import BitFoundation
@testable import bitchat

struct InteropVerifyTests {
    private struct EnvelopeCase: Decodable {
        let event: NostrEvent
        let recipient_private_key: String
        let expected_content: String
        let expected_sender: String
        let expected_timestamp: Int?
    }

    private struct EventCase: Decodable {
        let event: NostrEvent
        let min_pow: Int?
    }

    private struct EmbeddedCase: Decodable {
        let encoded: String
        let payload_type: UInt8
        let message_id: String
        let content: String?
        let sender: String?
        let recipient: String?
    }

    private func load<T: Decodable>(_ name: String, as type: T.Type) throws -> T? {
        guard let dir = ProcessInfo.processInfo.environment["BITCHAT_INTEROP_DIR"] else { return nil }
        let url = URL(fileURLWithPath: dir).appendingPathComponent("rust").appendingPathComponent(name)
        guard let data = try? Data(contentsOf: url) else { return nil }
        return try JSONDecoder().decode(type, from: data)
    }

    @Test func rustEnvelopesOpenInSwift() throws {
        guard let cases = try load("envelopes.json", as: [EnvelopeCase].self) else { return }
        #expect(!cases.isEmpty)
        for item in cases {
            let key = try #require(Data(hexString: item.recipient_private_key))
            let recipient = try NostrIdentity(privateKeyData: key)
            #expect(item.event.isValidSignature())
            let result = try NostrProtocol.decryptPrivateMessage(giftWrap: item.event, recipientIdentity: recipient)
            #expect(result.content == item.expected_content)
            #expect(result.senderPubkey == item.expected_sender)
            if let ts = item.expected_timestamp {
                #expect(result.timestamp == ts)
            }
        }
    }

    @Test func rustEventsVerifyInSwift() throws {
        guard let cases = try load("events.json", as: [EventCase].self) else { return }
        #expect(!cases.isEmpty)
        for item in cases {
            #expect(item.event.isValidSignature(), "event \(item.event.id)")
            if let minPow = item.min_pow {
                #expect(NostrPoW.validatedDifficulty(idHex: item.event.id, tags: item.event.tags) >= minPow)
            }
        }
    }

    @Test func rustEmbeddedPacketsDecodeInSwift() throws {
        guard let cases = try load("embedded.json", as: [EmbeddedCase].self) else { return }
        #expect(!cases.isEmpty)
        for item in cases {
            #expect(item.encoded.hasPrefix("bitchat1:"))
            let body = String(item.encoded.dropFirst("bitchat1:".count))
            let bytes = try #require(Base64URLCoding.decode(body))
            let packet = try #require(BitchatPacket.from(bytes))
            #expect(packet.type == MessageType.noiseEncrypted.rawValue)
            #expect(packet.senderID.hexEncodedString() == (item.sender ?? packet.senderID.hexEncodedString()))
            #expect(packet.recipientID?.hexEncodedString() == item.recipient)
            let payload = try #require(NoisePayload.decode(packet.payload))
            #expect(payload.type.rawValue == item.payload_type)
            if payload.type == .privateMessage {
                let message = try #require(PrivateMessagePacket.decode(from: payload.data))
                #expect(message.messageID == item.message_id)
                #expect(message.content == item.content)
            } else {
                #expect(String(data: payload.data, encoding: .utf8) == item.message_id)
            }
        }
    }
}
