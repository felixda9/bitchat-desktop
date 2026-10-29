// Checks output produced by the Rust client against the bitchat iOS code.
// Reads $BITCHAT_INTEROP_DIR/rust/*.json; each test is skipped when its file is missing.

import CryptoKit
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

    private struct MeshPacketCase: Decodable {
        let name: String
        let wire: String
        let noise_public: String
        let signing_public: String
        let nickname: String?
    }

    private struct GcsCase: Decodable {
        let ids: [String]
        let max_bytes: Int
        let p: Int
        let m: UInt32
        let data: String
        let included: Int
    }

    private struct RequestSyncCase: Decodable {
        let encoded: String
        let p: Int
        let m: UInt32
        let data: String
        let types: UInt64?
        let since: UInt64?
        let fragment_ids: String?
    }

    private struct MeshVectors: Decodable {
        let packets: [MeshPacketCase]
        let gcs: [GcsCase]
        let request_sync: [RequestSyncCase]
    }

    @Test func rustMeshPacketsVerifyInSwift() throws {
        guard let vectors = try load("mesh.json", as: MeshVectors.self) else { return }
        #expect(!vectors.packets.isEmpty)
        for item in vectors.packets {
            let wire = try #require(Data(hexString: item.wire))
            let packet = try #require(BitchatPacket.from(wire), "\(item.name)")
            let signature = try #require(packet.signature, "\(item.name)")
            let signingPublic = try #require(Data(hexString: item.signing_public))
            let key = try Curve25519.Signing.PublicKey(rawRepresentation: signingPublic)
            let preimage = try #require(packet.toBinaryDataForSigning())
            #expect(key.isValidSignature(signature, for: preimage), "\(item.name)")
            let noisePublic = try #require(Data(hexString: item.noise_public))
            #expect(packet.senderID.hexEncodedString() == PeerID(publicKey: noisePublic).id, "\(item.name)")
            if let nickname = item.nickname {
                let announce = try #require(AnnouncementPacket.decode(from: packet.payload), "\(item.name)")
                #expect(announce.nickname == nickname)
                #expect(announce.noisePublicKey == noisePublic)
                #expect(announce.signingPublicKey == signingPublic)
            }
        }

        for item in vectors.gcs {
            let data = item.data.isEmpty ? Data() : try #require(Data(hexString: item.data))
            let values = GCSFilter.decodeToSortedSet(p: item.p, m: item.m, data: data)
            #expect(item.included > 0)
            for idHex in item.ids.prefix(item.included) {
                let id = try #require(Data(hexString: idHex))
                let bucket = GCSFilter.bucket(for: id, modulus: item.m)
                #expect(GCSFilter.contains(sortedValues: values, candidate: bucket))
            }
            // Swift builds the same filter from the same IDs.
            let ids = try item.ids.map { try #require(Data(hexString: $0)) }
            let swift = GCSFilter.buildFilter(ids: ids, maxBytes: item.max_bytes, targetFpr: 0.01)
            #expect(swift.p == item.p && swift.m == item.m && swift.includedCount == item.included)
            #expect(swift.data.hexEncodedString() == item.data)
        }

        for item in vectors.request_sync {
            let bytes = try #require(Data(hexString: item.encoded))
            let request = try #require(RequestSyncPacket.decode(from: bytes))
            #expect(request.p == item.p)
            #expect(request.m == item.m)
            #expect(request.data.hexEncodedString() == item.data)
            #expect(request.types?.rawValue == item.types)
            #expect(request.sinceTimestamp == item.since)
            #expect(request.fragmentIdFilter == item.fragment_ids)
        }
    }
}
