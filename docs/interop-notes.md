# bitchat interop notes (research, pre-implementation)

Consolidated from reading the upstream sources on 2026-10-29. **Swift is the spec of record**;
where docs and code disagree, code wins. Paths are relative to `reference/bitchat/` unless
prefixed `android:` (= `reference/bitchat-android/app/src/main/java/com/bitchat/android/`).
`BF/` = `localPackages/BitFoundation/Sources/BitFoundation/`.

Pinned upstream: iOS `04995f6` (v1.8.0, 2026-10-25, Unlicense / public domain).
Android `20501fd` (v2.2.0, 2026-10-27, **GPL-3.0**: copying its code makes ours GPL).

All multi-byte integers are big-endian unless stated.

---

## 1. Identity

| Key | Type | Notes |
|---|---|---|
| Noise static | X25519, 32-byte raw | keychain `noiseStaticKey` |
| Signing | Ed25519, **separately generated** (not derived from Noise key) | keychain `ed25519SigningKey` |
| Nostr main | random secp256k1 (BIP-340 x-only pub) | not derived from the above; `bitchat/Nostr/NostrIdentity.swift` |
| Nostr per-geohash | `HMAC-SHA256(key=deviceSeed32, msg=utf8(geohash) ‖ BE32(i))`, i = 0..9 until valid secp256k1 scalar; fallback `SHA256(seed ‖ geohash)` | `bitchat/Nostr/NostrIdentityBridge.swift:98-140`. Bridge cells use `"bridge|" + cell` |

- Fingerprint = lowercase hex `SHA256(noise static pub)` (64 chars).
- Peer ID = first 8 bytes of that hash (16 lowercase hex). Stable; changes only on panic wipe.
  Peer-ID rotation (`BF/PeerIDRotation.swift`, announceV2 0x2C) is designed but **not shipped**.
- Noise↔Nostr link for mutual favorites: a private message `[FAVORITED]:npub1…` /
  `[UNFAVORITED]:npub1…` sent over a Noise session (mesh) or later over Nostr
  (`Services/BLE/BLEService.swift:2640-2655`, `Services/NostrTransport.swift:280-292`).
  **No mesh-free way to bootstrap a mutual favorite exists.**

## 2. Nostr layer (Phase 1)

### 2.1 Event basics
- NIP-01 id = SHA256 of `[0,pubkey,created_at,kind,tags,content]` JSON
  (iOS: `JSONSerialization` with `.withoutEscapingSlashes`). Unconfirmed edge: escaping of
  U+2028/U+2029 and other control chars vs NIP-01 canonical form; follow NIP-01 strictly.
- Schnorr BIP-340 with 32 random aux bytes. Verification required for every inbound event.
- Inbound tag limits: ≤64 tags, ≤16 values/tag, ≤1024 bytes/value (`TransportConfig.swift:105-107`).

### 2.2 Private envelope (kinds 1059 → 13 → 14). NOT NIP-17/44/59 compatible.
`bitchat/Nostr/NostrProtocol.swift`

1. **Rumor** (kind 14): `pubkey` = sender, `created_at` = real now (seconds), `tags` = `[]`
   (Android sends `[["p", recipient]]`), `content` = payload string, `id` = `""`, **no `sig`**.
   Serialized with `JSONEncoder` (keys id,pubkey,created_at,kind,tags,content; sig omitted).
2. **Seal** (kind 13): signed by the sender's real key, `tags` = `[]`,
   `created_at` = now ± uniform(−900…+900) s, `content` = encrypt(rumorJSON, sender key → recipient).
3. **Gift wrap** (kind 1059): fresh random key per message, `tags` = `[["p", recipientHex]]`,
   `created_at` = now ± 900 s (Android: 0–22 h in the past), `content` = encrypt(sealJSON, wrap key → recipient).

**encrypt(plaintext, senderPriv, recipientXonly)**
- Lift recipient x-only pubkey with **even Y** (`0x02‖x`).
- ECDH shared point S = senderPriv · P. IKM = **33-byte compressed S** (`02/03 ‖ x`), not the
  x-coordinate and not libsecp256k1's default SHA256-hashed output.
  (swift-secp256k1 0.24.0 `sharedSecretFromKeyAgreement(format: .compressed)`;
  Android matches: `android:nostr/NostrCrypto.kt:199-252`.)
- key = HKDF-SHA256(IKM, salt = empty, info = `"nip44-v2"`, L = 32) (RFC 5869 extract+expand).
- nonce = 24 random bytes. XChaCha20-Poly1305 (IETF/libsodium construction: HChaCha20 on
  nonce[0..16], then ChaCha20-Poly1305 with `00000000 ‖ nonce[16..24]`), **no AAD**.
- content = `"v2:" + base64url_nopad(nonce24 ‖ ciphertext ‖ tag16)`.

**decrypt**: require `v2:` prefix, total ≤ 64 KiB, decoded length > 40. Try sender pubkey lifted
with even Y, then odd Y (because the parity byte of S depends on it). Plaintext must be UTF-8.

**Inbound validation (iOS strict, `NostrProtocol.swift:98-181`)** — replicate exactly:
- outer: kind 1059, tags **exactly** `[["p", myPubHex]]`, valid signature, content ≤ 64 KiB.
- seal: kind 13, **no tags**, valid signature.
- rumor: kind 14, tags `[]` or `[["p", myPubHex]]`, `sig` absent, `rumor.pubkey == seal.pubkey`.
- authenticated sender = `seal.pubkey`; message time = `rumor.created_at`.
- plausible window: `now − rumorTs ∈ [−900 s, 86400 + 900 s]` (`NostrInboundPipeline.swift:586-591`).
- decrypted JSON layers bounded at 64 KiB before parsing.

### 2.3 DM content: `bitchat1:` embedded packet (`bitchat/Nostr/NostrEmbeddedBitChat.swift`)
`"bitchat1:" + base64url_nopad(BitchatPacket.toBinaryData())` where the packet is:
- v1, type `0x11` (noiseEncrypted) **but plaintext** (Nostr layer provides encryption), TTL 7,
  timestamp ms, unsigned, encoded with padding = true (and compression if it qualifies, §4).
- payload = `NoisePayloadType (1 byte) ‖ body`:
  - `0x01` privateMessage: TLV `00 len messageID` `01 len content`, **1-byte lengths, each ≤ 255
    UTF-8 bytes** (longer content fails to encode on iOS — no splitting exists). Decoder rejects
    unknown TLV types. Message IDs are `UUID().uuidString` (uppercase, hyphens); echo byte-for-byte.
  - `0x03` delivered / `0x02` readReceipt: body = UTF-8 messageID.
- Geohash DMs: senderID = 8 **random** bytes per envelope, no recipientID; signed/wrapped with the
  per-geohash identity of the current channel, sent to the recipient's per-geohash pubkey.
- Account (mutual-favorite) DMs: senderID = our peer ID, recipientID = their peer ID; wrapped with
  the main Nostr identity.
- Receiver auto-sends `delivered` for each private message; read receipts paced ~3/s.
- Inbound: `verify:` prefixed content ignored; non-`bitchat1:` content ignored.
- Payload types other than 01/02/03 are ignored over Nostr.

### 2.4 Location channels
- Geohash: standard base32 `0123456789bcdefghjkmnpqrstuvwxyz` (`Protocols/Geohash.swift`).
- Levels: region 2, province 4, city 5, neighborhood 6, block 7, building 8.
- **Chat** kind 20000: tags `["g", gh]`, optional `["n", nickname]`, optional `["t","teleport"]`,
  NIP-13 `["nonce", 16-lowercase-hex, "8"]` (iOS always mines 8 bits, 2 s cap, then steps down;
  `Nostr/NostrPoW.swift`). Signed by per-geohash key.
- **Presence** kind 20001: `[["g", gh]]` only, empty content, no nickname. Sent only for levels with
  precision ≤ 5, loop every 40–80 s, 2–5 s random gap between levels (`docs/GeohashPresenceSpec.md`).
- Online = pubkey seen (20000 or 20001) within 5 min. High-precision channels with 0 seen → `[? people]`.
- **Notes** kind 1 (`/drop`): `["g", gh]`, optional `n`, optional `["expiration", unix]`,
  optional `["t","urgent"]`. Deletion via kind 5 `["e", id]`.
- Teleport: tag `t=teleport` when posting to a geohash outside the device's physical regional set.
  A desktop without a location fix is effectively always teleported.
- Display: `nickname#<last 4 hex of pubkey>`.
- Validated PoW ≥ 8 bits relaxes per-sender rate limiting; iOS does NOT drop unmined events.
  (Android with PoW enabled drops < 12 bits — don't copy.)

### 2.5 Relays and subscriptions
- Geohash traffic: 5 relays nearest (haversine) to the geohash cell center, from
  `relays/online_relays_gps.csv` (`Relay URL,Latitude,Longitude`; refreshed daily from
  raw.githubusercontent.com/permissionlesstech/bitchat/refs/heads/main/relays/online_relays_gps.csv,
  with validation in `Nostr/GeoRelayDirectory.swift:442-560`). Android uses a different list (~60% overlap).
- DMs: built-in `wss://relay.damus.io`, `wss://nos.lol`, `wss://relay.primal.net`,
  `wss://offchain.pub` + up to 8 user-added (.onion allowed).
- Filters: geohash `{"kinds":[20000,20001],"#g":[gh],"since":now−3600,"limit":200}` on geo relays;
  DMs `{"kinds":[1059],"#p":[pub],"since":now−86400,"limit":100}` for the main key and for the
  current geohash key; notes `{"kinds":[1],"#g":[...],"limit":200}`.
- Backoff 1 s ×2 to 300 s, 20 % jitter, 10 attempts; EOSE fallback 10 s.
- **Tor default on, fail-closed** (Arti in-process). Toggle exists; off = clearnet.

## 3. IRC commands and panic wipe
- iOS commands (`Services/CommandProcessor.swift:124-158`): `/m /msg`, `/w /who`, `/clear`,
  `/hug`, `/slap`, `/block`, `/unblock`, `/fav`, `/unfav`, `/ping`, `/trace`, `/group`, `/pay`,
  `/drop`, `/help`. Geohash contexts reject /fav /unfav /ping /trace /group.
- Emotes are plain text: `* 🫂 alice hugs bob *`, `* 🐟 alice slaps bob around a bit with a large trout *`
  (format `"* \(emoji) \(me) \(action) \(target)\(suffix) *"`; in a DM the target reads "you").
- Panic wipe: triple-tap the `bitchat/` header logo → confirmation dialog → wipe
  (`App/AppChromeModel.swift:123-145`, `ViewModels/ChatViewModel.swift:1561+`). Wipes: all keys
  (new Noise + Ed25519 → new peer ID, new Nostr key + seed), messages, favorites, blocks, location
  state/bookmarks, user-added relays, outbox/courier/archives, nickname → `anon####`.

## 4. Packet frame (`BF/BinaryProtocol.swift`)
- v1 header 14 B: version(1)=1, type, ttl, timestamp u64 ms, flags, payloadLength u16.
  v2 header 16 B: same with payloadLength u32.
- Body: senderID 8 ‖ recipientID 8 (flag 0x01) ‖ route (v2 + flag 0x08: count u8 + count×8) ‖
  originalSize (flag 0x04: u16 v1 / u32 v2) ‖ payload ‖ signature 64 (flag 0x02).
- payloadLength = payload bytes (+ originalSize field when compressed).
- Flags: 0x01 recipient, 0x02 signature, 0x04 compressed, 0x08 route (v2 only), 0x10 RSR.
- Decoder: min 22 B; try raw decode first, then strip PKCS#7 padding and retry; trailing bytes ignored.
- Compression: raw DEFLATE (Apple `COMPRESSION_ZLIB`, documented as zlib level 5), attempted when
  payload ≥ 100 B and distinct-byte ratio < 0.9; kept only if strictly shorter.
- Padding: smallest of 256/512/1024/2048 ≥ len+16; PKCS#7; only if pad 1..255. On BLE only
  0x10/0x11 are padded. `toBinaryData()` defaults to padded.
- Per-type payload caps: `BF/PacketPayloadLimits.swift:58-97`.
- **Signature pre-image** (`BF/BitchatPacket.swift:51-67`): re-encode the *decoded* packet with
  signature=nil, TTL=0, RSR=false, **padding=true, compression re-applied by the verifier's own
  encoder**. ⇒ our deflate output must be byte-identical to Apple's for signed payloads that
  qualify for compression. **Top mesh interop risk; needs macOS-generated vectors.**
- Ed25519 signatures are randomized on iOS: pin pre-images, not signature bytes.

## 5. Mesh message types (`BF/MessageType.swift`)
- 0x01 announce: TLV (u8 type, u8 len): 01 nickname ≤255, 02 Noise pub 32, 03 Ed25519 pub 32,
  04 neighbors N×8 (≤10), 05 capabilities (LE minimal bitfield; iOS sends `29 03`), 06 bridge geohash.
  v1, TTL 7, signed. Receiver checks senderID == SHA256(noise)[0:8], age ≤ 900 s, signature, TOFU pins.
  TTL 7 on receipt = direct neighbor.
- 0x02 message: payload = raw UTF-8 text. v1, TTL 7, signed. Accepted only from verified-announce peers.
  Stable ID = hex(SHA256(lowercase senderHex|timestampMs|trimmed content))[0:32].
- 0x03 leave: empty, signed, unpadded.
- 0x10 noiseHandshake / 0x11 noiseEncrypted: see §6. Directed, unsigned, padded.
- 0x20 fragment: fragmentID 8 random ‖ index u16 ‖ total u16 ‖ originalType u8 ‖ chunk of the
  original encoded frame. Default chunk 469 B. Reassembly key (sender, fragmentID), total 1..10000,
  128 in flight, 30 s timeout.
- 0x21 requestSync: TLV (u8 type, u16 len): 01 P, 02 M u32, 03 GCS bytes, 04 type flags (LE),
  05 since u64 ms, 06 fragment-ID list. v1, TTL 0, signed, never relayed. GCS: P=7 (fpr 0.01),
  400-byte budget, h = SHA256(packetID)[0:8] & 0x7FFF…, v = h mod M (0→1), Golomb-Rice deltas.
  packetID = SHA256(type ‖ sender ‖ ts u64 ‖ payload)[0:16]. Replies: original packets, TTL 0, RSR set.
- 0x22 fileTransfer: TLV 01 name (u16 len), 02 size (len 4, u32), 03 mime, 04 content (**u32 len**).
  v2, signed, always fragmented, ≤ 1 MiB.
- 0x26 ping / 0x27 pong: nonce 8 ‖ originTTL u8.
- iOS-only, later phases: 0x04 courier, 0x23 board, 0x24 prekeyBundle, 0x25 group, 0x28 nostrCarrier,
  0x29 voice. Unknown types are decoded and flood-relayed.

## 6. Noise (`bitchat/Noise/NoiseProtocol.swift`, `NoiseSessionManager.swift`)
- `Noise_XX_25519_ChaChaPoly_SHA256`, empty prologue, standard SymmetricState/HKDF/Split.
  ChaCha nonce = 4 zero bytes ‖ LE64 counter (standard).
- Handshake payloads empty: msg1 = 32 B (**must be exactly 32**), msg2 = 96 B, msg3 = 64 B.
- Extra pubkey blacklist on received e/s (`NoiseProtocol.swift:984-1039`) — port exactly.
- **Transport frame (non-standard):** `BE32(nonce) ‖ ciphertext ‖ tag16`; receiver decrypts with
  the received nonce; 1024-nonce sliding replay window; empty AD; sender refuses n > 2³²−2.
  (iOS's window bitmap has a shift bug — implement a correct window with same size/boundary.)
  Maps onto snow's `StatelessTransportState`.
- Identity binding: SHA256(remote static)[0:8] must equal the header senderID.
- Tie-break on crossed msg1: lower peer ID (bytewise = iOS lowercase-hex string order) keeps
  initiator; higher yields.
- "Fresh initiation" = 32 bytes, or anything when no session exists, or > 32 bytes while
  established. Shorter bytes for an established session are ignored.
- msg1 while established = re-handshake; old session kept receive-only until done (20 s). If it
  fails or times out the old keys come back, further msg1s are ignored for 60 s, and (on
  timeout) sending waits for one recovery handshake of our own.
- A crossed msg1 that arrives < 1 s after we finished as initiator (we are lower) is ignored;
  we then re-handshake once after the 1 s.
- Initiator rekey drops the old session at once (no receive-only copy).
- Ciphertext with no session triggers a handshake; ciphertext while we wait for msg3 is held
  (4/peer, 32 total, 20 s) and replayed after.
- "Rekey" = new XX handshake (age > 21.6 h, idle > 24 h, or ≥ 9e8 messages).
- Timeouts: initiator 10 s, responder 20 s. Rate limits: handshake messages 10/min/peer,
  30/min total; incoming ciphertext 100/s/peer, 500/s total.
- After establishment iOS sends 0x21 authenticatedPeerState, queued payloads, then an announce.
  Each side echoes the state once after receiving the other's.
- Inner payload types: 01 PM, 02 read, 03 delivered, 06/07 group, 08 voice, 10/11 verify,
  12 vouch, 20 privateFile (09 alias), 21 authenticatedPeerState
  (`01 [01 len caps] [02 20 ed25519pub]`).
- QR verify: challenge `01 len noiseKeyHex 02 len nonceA(16)`; response adds `03 len sig`,
  sig = Ed25519("bitchat-verify-resp-v1" ‖ len(nk) ‖ nk ‖ nonceA).

## 7. BLE / GATT
- Service UUID `F47B5E2D-4A9E-4C5A-9B3F-8E1D2C3A4B5C` (release); `…4B5A` in iOS DEBUG builds.
- Characteristic `A1B2C3D4-E5F6-4A5B-8C9D-0E1F2A3B4C5D`: notify, write, writeWithoutResponse, read.
- Advertisement: service UUID only, **no local name**. Backgrounded iPhones hide the UUID in Apple
  overflow data ⇒ we must advertise as a peripheral so they can find us.
- iOS writes are always write-without-response, one whole frame per write. Notifications may span
  frames; iOS reassembles a byte stream (250 ms stall reset).
- Announce on every new link and every ~15–30 s; iOS forgets peers after 45–60 s. Clock within ±120 s.
- Relay: TTL 7 origin; dedup key `sender-timestamp-type-hex(SHA256(payload)[0:4])`, 300 s / 1000;
  degree-based TTL clamps and jitter (`Services/RelayController.swift:26-95`) — local policy.
- Source routing (v2): originate only if every hop has been seen sending v2. Parse routes if we send v2.

## 8. Courier / prekeys (later phase)
- Seal: `Noise_X_25519_ChaChaPoly_SHA256`, prologue `"bitchat-courier-v1"`; prekey variant
  prologue `"bitchat-prekey-v1" ‖ BE32(prekeyID)`.
- Recipient tag = HMAC-SHA256(key = recipient noise static pub, `"bitchat-courier-tag-v1" ‖ BE32(day)`)[0:16].
- Envelope TLV (u8 type, u16 len): 01 tag, 02 expiry u64 ms, 03 ciphertext, 04 copies, 05 prekeyID.
- Vectors: `docs/courier-test-vectors.json`.

## 9. Existing fixtures to import
- `bitchatTests/Nostr/Fixtures/LegacyPrivateEnvelope733098bb*.json` (iOS-generated envelope + recipient key)
- `bitchatTests/Nostr/Fixtures/AndroidLegacyPrivateEnvelopeB7f0b33d*.json` (Android-generated, recipient key = 2)
- `bitchatTests/Noise/NoiseTestVectors.json` (cacophony/snow XX, standard framing only)
- `docs/courier-test-vectors.json` (frame layout, padded signing pre-image, Ed25519 seed 0x42×32)
- `localPackages/BitFoundation/Tests/BitFoundationTests/BinaryProtocolTests.swift` (incl. malformed frames)
- android: `app/src/test/kotlin/com/bitchat/android/contracts/ClientRewriteWireContractTest.kt` (12 hex vectors)
- Missing (generate from Swift on macOS): embedded `bitchat1:` packets, extracted-nonce transport
  frames, deflate output of Apple's encoder, signed announce/message pre-images, per-geohash key
  derivation, PoW, event-ID serialization edge cases.

## 10. Known divergences / upstream issues to remember
- Android: no RSR flag on sync replies (iOS drops them if > 120 s old); deflate level 6 vs Apple;
  256-fragment cap; drops geohash chat < 12 PoW bits when PoW enabled; different geo relay list;
  hand-rolled non-constant-time Schnorr; `/w` but no `/who`.
- iOS: replay-window bit-shift bug; possible off-by-one in directed fragment sizing (unverified);
  BRING_THE_NOISE.md largely outdated (no version negotiation; 0x20/0x21 are fragment/requestSync).
- Windows `GattServiceProvider` puts the service UUID in the advertisement only with
  `IsDiscoverable = true`, which also broadcasts the PC's Bluetooth name (phones send no name).
