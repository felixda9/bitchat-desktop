// Exports interop vectors from the bitchat iOS code.
// Copied into bitchatTests/Interop/ of an upstream checkout by .github/workflows/swift-vectors.yml.
// Output: $BITCHAT_INTEROP_DIR/swift/export.json

import CryptoKit
import Foundation
import P256K
import Testing
@testable import BitFoundation
@testable import bitchat

struct InteropExportTests {
    @Test func exportVectors() async throws {
        guard let dir = ProcessInfo.processInfo.environment["BITCHAT_INTEROP_DIR"] else { return }

        var out: [String: Any] = [:]
        out["geohash_identities"] = try geohashIdentities()
        out["event_ids"] = try eventIDs()
        out["pow"] = powCases()
        out["mined_event"] = try await minedEvent()
        out["envelope_chain"] = try envelopeChain()
        out["envelopes"] = try envelopes()
        out["embedded"] = embedded()
        out["packets"] = packets()
        out["private_message"] = privateMessages()
        out["compression"] = compression()
        out["geohash"] = geohash()

        let folder = URL(fileURLWithPath: dir).appendingPathComponent("swift")
        try FileManager.default.createDirectory(at: folder, withIntermediateDirectories: true)
        let data = try JSONSerialization.data(
            withJSONObject: out,
            options: [.prettyPrinted, .sortedKeys, .withoutEscapingSlashes]
        )
        try data.write(to: folder.appendingPathComponent("export.json"))
    }

    // MARK: - Helpers

    private func key(_ last: UInt8) throws -> NostrIdentity {
        var bytes = [UInt8](repeating: 0, count: 32)
        bytes[31] = last
        return try NostrIdentity(privateKeyData: Data(bytes))
    }

    private func eventDict(_ event: NostrEvent) -> [String: Any] {
        [
            "id": event.id,
            "pubkey": event.pubkey,
            "created_at": event.created_at,
            "kind": event.kind,
            "tags": event.tags,
            "content": event.content,
            "sig": event.sig ?? ""
        ]
    }

    private func optionalHex(_ data: Data?) -> Any {
        guard let data else { return NSNull() }
        return data.hexEncodedString()
    }

    private func optionalString(_ value: String?) -> Any {
        guard let value else { return NSNull() }
        return value
    }

    // MARK: - Identities and events

    private func geohashIdentities() throws -> [String: Any] {
        let seed = Data((0..<32).map { UInt8($0) })
        let keychain = MockKeychain()
        keychain.save(key: "nostr-device-seed", data: seed, service: "chat.bitchat.nostr", accessible: nil)
        let bridge = NostrIdentityBridge(keychain: keychain)

        var rows: [[String: Any]] = []
        for geohash in ["u", "u4", "u4pr", "u4pru", "u4pruy", "u4pruydq", "9q8yyk8y", "dr5regw3pp"] {
            let identity = try bridge.deriveIdentity(forGeohash: geohash)
            rows.append([
                "label": geohash,
                "private_key": identity.privateKey.hexEncodedString(),
                "public_key": identity.publicKeyHex,
                "npub": identity.npub
            ])
        }
        let rendezvous = try bridge.deriveIdentity(forBridgeRendezvous: "u4pru")
        rows.append([
            "label": "bridge|u4pru",
            "private_key": rendezvous.privateKey.hexEncodedString(),
            "public_key": rendezvous.publicKeyHex,
            "npub": rendezvous.npub
        ])
        return ["seed": seed.hexEncodedString(), "identities": rows]
    }

    private func eventIDs() throws -> [[String: Any]] {
        let identity = try key(3)
        let contents = [
            "hello",
            "",
            "line\nbreak",
            "tab\tcr\r",
            "quote \" backslash \\",
            "slash / ok",
            "emoji 🫂🐟",
            "é ß 中文",
            "ls\u{2028}ps\u{2029}",
            "ctrl\u{01}\u{1f}\u{7f}",
            "bs\u{08}ff\u{0c}"
        ]
        var rows: [[String: Any]] = []
        for content in contents {
            let event = NostrEvent(
                pubkey: identity.publicKeyHex,
                createdAt: Date(timeIntervalSince1970: 1_790_000_000),
                kind: .ephemeralEvent,
                tags: [["g", "u4pruy"], ["n", "alice"]],
                content: content
            )
            let signed = try event.sign(with: identity.schnorrSigningKey())
            #expect(signed.isValidSignature())
            rows.append(eventDict(signed))
        }
        return rows
    }

    private func powCases() -> [[String: Any]] {
        let ff = String(repeating: "f", count: 60)
        let cases: [(String, [[String]])] = [
            ("00ff" + ff, [["nonce", "1", "8"]]),
            ("00ff" + ff, [["nonce", "1", "9"]]),
            ("0fff" + ff, [["nonce", "1", "4"]]),
            ("0000" + ff, [["nonce", "1", "8"], ["nonce", "2", "16"]]),
            ("0000" + ff, [["nonce", "2", "16"], ["nonce", "1", "8"]]),
            ("00ff" + ff, []),
            ("00ff" + ff, [["nonce", "1"]]),
            ("00ff" + ff, [["nonce", "1", "0"]]),
            ("00ff" + ff, [["nonce", "1", "abc"]]),
            ("00ff" + ff, [["nonce", "1", "300"]]),
            (String(repeating: "0", count: 64), [["nonce", "1", "256"]])
        ]
        return cases.map { (idHex, tags) -> [String: Any] in
            [
                "id": idHex,
                "tags": tags,
                "difficulty": NostrPoW.validatedDifficulty(idHex: idHex, tags: tags)
            ]
        }
    }

    private func minedEvent() async throws -> [String: Any] {
        let identity = try key(4)
        let event = try await NostrProtocol.createMinedEphemeralGeohashEvent(
            content: "mined hello",
            geohash: "u4pruy",
            senderIdentity: identity,
            nickname: "alice",
            teleported: true
        )
        var row = eventDict(event)
        row["validated_difficulty"] = NostrPoW.validatedDifficulty(idHex: event.id, tags: event.tags)
        return row
    }

    // MARK: - Private envelope

    private func compressedPublicKey(_ identity: NostrIdentity) throws -> Data {
        let agreement = try P256K.KeyAgreement.PrivateKey(dataRepresentation: identity.privateKey)
        return agreement.publicKey.dataRepresentation
    }

    private func sharedPoint(privateKey: Data, xOnly: Data, prefix: UInt8) throws -> Data {
        let agreement = try P256K.KeyAgreement.PrivateKey(dataRepresentation: privateKey)
        let publicKey = try P256K.KeyAgreement.PublicKey(
            dataRepresentation: Data([prefix]) + xOnly,
            format: .compressed
        )
        let secret = agreement.sharedSecretFromKeyAgreement(with: publicKey, format: .compressed)
        return secret.withUnsafeBytes { Data($0) }
    }

    private func envelopeKey(_ shared: Data) -> Data {
        let derived = HKDF<CryptoKit.SHA256>.deriveKey(
            inputKeyMaterial: SymmetricKey(data: shared),
            salt: Data(),
            info: Data("nip44-v2".utf8),
            outputByteCount: 32
        )
        return derived.withUnsafeBytes { Data($0) }
    }

    private func envelopeChain() throws -> [[String: Any]] {
        let nonce = Data((0x10..<0x28).map { UInt8($0) })
        var rows: [[String: Any]] = []
        for (s, r) in [(UInt8(1), UInt8(2)), (3, 7), (5, 6), (8, 9), (4, 1)] {
            let sender = try key(s)
            let recipient = try key(r)
            let senderShared = try sharedPoint(privateKey: sender.privateKey, xOnly: recipient.publicKey, prefix: 0x02)
            let recipientEven = try sharedPoint(privateKey: recipient.privateKey, xOnly: sender.publicKey, prefix: 0x02)
            let recipientOdd = try sharedPoint(privateKey: recipient.privateKey, xOnly: sender.publicKey, prefix: 0x03)
            let symmetric = envelopeKey(senderShared)
            let plaintext = "chain test \(s)->\(r)"
            let sealed = try XChaCha20Poly1305Compat.seal(
                plaintext: Data(plaintext.utf8),
                key: symmetric,
                nonce24: nonce
            )
            let senderCompressed = try compressedPublicKey(sender)
            let recipientCompressed = try compressedPublicKey(recipient)
            rows.append([
                "sender_private_key": sender.privateKey.hexEncodedString(),
                "sender_public_key_compressed": senderCompressed.hexEncodedString(),
                "recipient_private_key": recipient.privateKey.hexEncodedString(),
                "recipient_public_key_compressed": recipientCompressed.hexEncodedString(),
                "sender_shared_point": senderShared.hexEncodedString(),
                "recipient_shared_point_even": recipientEven.hexEncodedString(),
                "recipient_shared_point_odd": recipientOdd.hexEncodedString(),
                "key": symmetric.hexEncodedString(),
                "nonce": nonce.hexEncodedString(),
                "plaintext": plaintext,
                "ciphertext": sealed.ciphertext.hexEncodedString(),
                "tag": sealed.tag.hexEncodedString(),
                "content": "v2:" + Base64URLCoding.encode(nonce + sealed.ciphertext + sealed.tag)
            ])
        }
        return rows
    }

    private func envelopes() throws -> [[String: Any]] {
        let embeddedPM = NostrEmbeddedBitChat.encodePMForNostrNoRecipient(
            content: "geohash dm from swift",
            messageID: "6F9619FF-8B86-D011-B42D-00C04FC964FF"
        ) ?? ""
        let contents = ["hello from swift", "héllo 🫂 / \"quoted\"", embeddedPM]
        var rows: [[String: Any]] = []
        for (index, content) in contents.enumerated() {
            let sender = try key(UInt8(10 + index))
            let recipient = try key(UInt8(20 + index))
            let event = try NostrProtocol.createPrivateMessage(
                content: content,
                recipientPubkey: recipient.publicKeyHex,
                senderIdentity: sender
            )
            rows.append([
                "event": eventDict(event),
                "recipient_private_key": recipient.privateKey.hexEncodedString(),
                "expected_content": content,
                "expected_sender": sender.publicKeyHex
            ])
        }
        return rows
    }

    // MARK: - Packets

    private func embedded() -> [[String: Any]] {
        let sender = PeerID(str: "0102030405060708")
        let recipient = PeerID(str: "1112131415161718")
        let messageID = "6F9619FF-8B86-D011-B42D-00C04FC964FF"
        let long = String(repeating: "abc ", count: 50)
        var rows: [[String: Any]] = []

        func add(_ name: String, _ encoded: String?) {
            rows.append(["name": name, "encoded": optionalString(encoded)])
        }
        add("pm", NostrEmbeddedBitChat.encodePMForNostr(content: "hi there", messageID: messageID, recipientPeerID: recipient, senderPeerID: sender))
        add("pm_long", NostrEmbeddedBitChat.encodePMForNostr(content: long, messageID: messageID, recipientPeerID: recipient, senderPeerID: sender))
        add("delivered", NostrEmbeddedBitChat.encodeAckForNostr(type: .delivered, messageID: messageID, recipientPeerID: recipient, senderPeerID: sender))
        add("read", NostrEmbeddedBitChat.encodeAckForNostr(type: .readReceipt, messageID: messageID, recipientPeerID: recipient, senderPeerID: sender))
        add("geo_pm", NostrEmbeddedBitChat.encodePMForNostrNoRecipient(content: "hi geo", messageID: messageID))
        add("geo_delivered", NostrEmbeddedBitChat.encodeAckForNostrNoRecipient(type: .delivered, messageID: messageID))
        add("geo_read", NostrEmbeddedBitChat.encodeAckForNostrNoRecipient(type: .readReceipt, messageID: messageID))
        return rows
    }

    private func packets() -> [[String: Any]] {
        let sender = Data(hexString: "0102030405060708")!
        let recipient = Data(hexString: "1112131415161718")!
        let signature = Data(repeating: 0xAB, count: 64)
        let route = [Data(hexString: "2122232425262728")!, Data(hexString: "3132333435363738")!]
        let text = Data(String(repeating: "hello world ", count: 25).utf8)

        struct Case {
            let name: String
            let packet: BitchatPacket
        }
        var cases: [Case] = [
            Case(name: "message", packet: BitchatPacket(type: 0x02, senderID: sender, recipientID: nil, timestamp: 1_790_000_000_123, payload: Data("hi".utf8), signature: nil, ttl: 7)),
            Case(name: "message_compressible", packet: BitchatPacket(type: 0x02, senderID: sender, recipientID: nil, timestamp: 1_790_000_000_123, payload: text, signature: nil, ttl: 7)),
            Case(name: "signed_directed", packet: BitchatPacket(type: 0x11, senderID: sender, recipientID: recipient, timestamp: 42, payload: Data([1, 2, 3]), signature: signature, ttl: 5)),
            Case(name: "v2_route", packet: BitchatPacket(type: 0x11, senderID: sender, recipientID: recipient, timestamp: 42, payload: Data([0xde, 0xad]), signature: nil, ttl: 6, version: 2, route: route)),
            Case(name: "rsr", packet: BitchatPacket(type: 0x02, senderID: sender, recipientID: recipient, timestamp: 7, payload: Data("sync".utf8), signature: signature, ttl: 0, isRSR: true))
        ]
        for size in [1, 200, 218, 219, 220, 240, 241, 400, 474, 475, 1000, 2000, 2010, 2040] {
            let payload = Data((0..<size).map { UInt8(($0 * 131 + 7) % 251) })
            cases.append(Case(name: "noise_\(size)", packet: BitchatPacket(type: 0x11, senderID: sender, recipientID: recipient, timestamp: 1, payload: payload, signature: nil, ttl: 7)))
        }

        return cases.map { item -> [String: Any] in
            let p = item.packet
            var route: Any = NSNull()
            if let hops = p.route {
                route = hops.map { $0.hexEncodedString() }
            }
            return [
                "name": item.name,
                "version": Int(p.version),
                "type": Int(p.type),
                "ttl": Int(p.ttl),
                "timestamp": NSNumber(value: p.timestamp),
                "sender": p.senderID.hexEncodedString(),
                "recipient": optionalHex(p.recipientID),
                "route": route,
                "payload": p.payload.hexEncodedString(),
                "signature": optionalHex(p.signature),
                "is_rsr": p.isRSR,
                "padded": optionalHex(p.toBinaryData(padding: true)),
                "unpadded": optionalHex(p.toBinaryData(padding: false)),
                "signing": optionalHex(p.toBinaryDataForSigning())
            ]
        }
    }

    private func privateMessages() -> [[String: Any]] {
        let cases = [
            ("m1", "hi"),
            ("6F9619FF-8B86-D011-B42D-00C04FC964FF", "héllo 🫂"),
            ("id", String(repeating: "x", count: 255)),
            ("id", String(repeating: "x", count: 256)),
            ("id", String(repeating: "é", count: 128))
        ]
        return cases.map { (id, content) -> [String: Any] in
            [
                "message_id": id,
                "content": content,
                "tlv": optionalHex(PrivateMessagePacket(messageID: id, content: content).encode())
            ]
        }
    }

    private func compression() -> [[String: Any]] {
        let inputs: [Data] = [
            Data(repeating: 0x61, count: 99),
            Data(repeating: 0x61, count: 100),
            Data(String(repeating: "hello world ", count: 25).utf8),
            Data("The quick brown fox jumps over the lazy dog. The quick brown fox jumps over the lazy dog again.".utf8),
            Data((0..<300).map { UInt8($0 % 256) }),
            Data((0..<1000).map { UInt8(($0 * 7) % 13) }),
            Data(String(repeating: "abc ", count: 50).utf8)
        ]
        return inputs.map { input -> [String: Any] in
            [
                "input": input.hexEncodedString(),
                "should_compress": CompressionUtil.shouldCompress(input),
                "compressed": optionalHex(CompressionUtil.compress(input))
            ]
        }
    }

    private func geohash() -> [String: Any] {
        let points: [(Double, Double)] = [
            (48.8566, 2.3522), (40.7128, -74.0060), (-33.8688, 151.2093),
            (0, 0), (89.99, 179.99), (-89.99, -179.99), (35.6762, 139.6503)
        ]
        var encoded: [[String: Any]] = []
        for (lat, lon) in points {
            for precision in [1, 2, 4, 5, 6, 7, 8, 12] {
                encoded.append([
                    "lat": lat,
                    "lon": lon,
                    "precision": precision,
                    "geohash": Geohash.encode(latitude: lat, longitude: lon, precision: precision)
                ])
            }
        }
        var decoded: [[String: Any]] = []
        for hash in ["u", "u4", "u4pruy", "9q8yyk8y", "dr5regw3pp", "zzzzzz", "000000", "b", "ezs42"] {
            let center = Geohash.decodeCenter(hash)
            decoded.append([
                "geohash": hash,
                "center_lat": center.lat,
                "center_lon": center.lon,
                "neighbors": Geohash.neighbors(of: hash),
                "valid": Geohash.isValidGeohash(hash)
            ])
        }
        let validity = ["", "u4pruy", "U4PRUY", "u4pruya", "u4pruyi", "#u4pruy", "0123456789bcd"].map {
            ["geohash": $0, "valid": Geohash.isValidGeohash($0)] as [String: Any]
        }
        return ["encode": encoded, "decode": decoded, "validity": validity]
    }
}
