//! IRC-style commands (iOS `CommandProcessor`), location-channel context.

use crate::engine::Engine;

pub const HELP: &str = "commands:\n\
/msg @name [message] — start a private chat\n\
/who — list who's here\n\
/clear — clear this chat\n\
/hug @name — send a hug\n\
/slap @name — slap with a large trout\n\
/block @name · /unblock @name\n\
/fav @name · /unfav @name — favorites (mesh only)\n\
/group create <name> — start an encrypted group\n\
/group invite @name · /group remove @name — manage members (creator)\n\
/group leave · /group list — leave or list your groups\n\
/ping @name — measure round-trip time (mesh only)\n\
/trace @name — estimated mesh path (mesh only)\n\
/pay <token> — send a cashu ecash token in this chat\n\
/drop <message> — pin a note to this place for 24h (needs location)\n\
/help — this list";

pub fn run(engine: &mut Engine, input: &str, now_ms: u64) {
    let (cmd, args) = match input.split_once(' ') {
        Some((c, a)) => (c, a),
        None => (input, ""),
    };
    match cmd {
        "/m" | "/msg" => msg(engine, args, now_ms),
        "/w" | "/who" => who(engine, now_ms),
        "/clear" => engine.clear_current(),
        "/hug" => emote(engine, args, "hug", "hugs", "🫂", "", now_ms),
        "/slap" => emote(
            engine,
            args,
            "slap",
            "slaps",
            "🐟",
            " around a bit with a large trout",
            now_ms,
        ),
        "/block" => block(engine, args, now_ms),
        "/unblock" => unblock(engine, args, now_ms),
        "/group" => engine.reply("groups are only for mesh peers in #mesh"),
        "/fav" | "/unfav" => engine.reply("favorites are only for mesh peers in #mesh"),
        "/ping" => engine.reply("ping only works for mesh peers in #mesh"),
        "/trace" => engine.reply("trace only works for mesh peers in #mesh"),
        "/pay" => engine.reply("cashu payments aren't supported in the desktop client yet"),
        "/drop" => drop(engine, args, now_ms),
        "/help" => engine.reply(HELP),
        other => engine.reply(format!(
            "unknown command: {other} — type /help for commands"
        )),
    }
}

fn msg(engine: &mut Engine, args: &str, now_ms: u64) {
    let (target, message) = match args.split_once(' ') {
        Some((t, m)) => (t, Some(m)),
        None => (args, None),
    };
    if target.is_empty() {
        engine.reply("usage: /msg @nickname [message]");
        return;
    }
    let nickname = target.strip_prefix('@').unwrap_or(target);
    let Some(peer) = engine.resolve_nickname(nickname, now_ms) else {
        engine.reply(format!("'{nickname}' not found"));
        return;
    };
    engine.open_dm(Some(&peer));
    if let Some(m) = message.map(str::trim).filter(|m| !m.is_empty()) {
        engine.send_dm(&peer, m, now_ms);
    }
    engine.reply(format!("started private chat with {nickname}"));
}

fn who(engine: &mut Engine, now_ms: u64) {
    if !engine.in_channel() {
        engine.reply("nobody around");
        return;
    }
    let mut names = engine.people_names(now_ms);
    if names.is_empty() {
        engine.reply("no one else is online right now");
        return;
    }
    names.sort();
    engine.reply(format!("online: {}", names.join(", ")));
}

fn emote(
    engine: &mut Engine,
    args: &str,
    command: &str,
    action: &str,
    emoji: &str,
    suffix: &str,
    now_ms: u64,
) {
    let target = args.trim();
    if target.is_empty() {
        engine.reply(format!("usage: /{command} <nickname>"));
        return;
    }
    let nickname = target.strip_prefix('@').unwrap_or(target);
    let Some(peer) = engine.resolve_nickname(nickname, now_ms) else {
        engine.reply(format!("cannot {command} {nickname}: not found"));
        return;
    };
    let me = engine.state.nickname.clone();
    if engine.open_dm_peer().is_some() {
        engine.send_dm(
            &peer,
            &format!("* {emoji} {me} {action} you{suffix} *"),
            now_ms,
        );
        let past = match action {
            "hugs" => "hugged".to_string(),
            "slaps" => "slapped".to_string(),
            a if a.ends_with('e') => format!("{a}d"),
            a => format!("{a}ed"),
        };
        engine.reply(format!("{emoji} you {past} {nickname}{suffix}"));
    } else {
        engine.send_public(
            &format!("* {emoji} {me} {action} {nickname}{suffix} *"),
            now_ms,
            false,
        );
        engine.reply(format!("{emoji} {me} {action} {nickname}{suffix}"));
    }
}

fn block(engine: &mut Engine, args: &str, now_ms: u64) {
    let target = args.trim();
    if target.is_empty() {
        let mut names = engine.blocked_names();
        names.sort();
        let geo = if names.is_empty() {
            "none".to_string()
        } else {
            names.join(", ")
        };
        engine.reply(format!("blocked peers: none | geohash blocks: {geo}"));
        return;
    }
    let nickname = target.strip_prefix('@').unwrap_or(target);
    let Some(pk) = engine.pubkey_for_display_name(nickname, now_ms) else {
        engine.reply(format!(
            "cannot block {nickname}: not found or unable to verify identity"
        ));
        return;
    };
    if engine.state.blocked.contains(&pk) {
        engine.reply(format!("{nickname} is already blocked"));
        return;
    }
    engine.set_blocked(&pk, true);
    engine.reply(format!("blocked {nickname} in geohash chats"));
}

fn unblock(engine: &mut Engine, args: &str, now_ms: u64) {
    let target = args.trim();
    if target.is_empty() {
        engine.reply("usage: /unblock <nickname>");
        return;
    }
    let nickname = target.strip_prefix('@').unwrap_or(target);
    // Blocked people are hidden, so also match the names shown by /block.
    let pk = engine
        .pubkey_for_display_name(nickname, now_ms)
        .or_else(|| {
            engine
                .state
                .blocked
                .iter()
                .find(|pk| engine.display_of(pk) == nickname)
                .cloned()
        });
    let Some(pk) = pk else {
        engine.reply(format!("cannot unblock {nickname}: not found"));
        return;
    };
    if !engine.state.blocked.contains(&pk) {
        engine.reply(format!("{nickname} is not blocked"));
        return;
    }
    engine.set_blocked(&pk, false);
    engine.reply(format!("unblocked {nickname} in geohash chats"));
}

fn drop(engine: &mut Engine, args: &str, now_ms: u64) {
    let text = args.trim();
    if text.is_empty() {
        engine.reply("usage: /drop <message>");
        return;
    }
    match engine.drop_note(text, now_ms) {
        Ok(()) => engine.reply("📍 note left here — it fades in 24h"),
        Err(e) => engine.reply(e),
    }
}
