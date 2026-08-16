/**
 * Server capabilities reported by `/api/health` — the UI hides what the server
 * says isn't there, and the matching endpoints refuse as defense in depth.
 *
 * `followups` (spec 007, #444, #471): the global follow-up inbox is **opt-in**
 * via `CEZ_FOLLOWUPS=1` and off by default. Off, agents are never told to write
 * `todos.json` (they get `HANDOFF_ONLY_INSTRUCTIONS` and an empty
 * `CEZ_TODOS_FILE`), the Inbox nav item is gone and the inbox endpoints refuse.
 * The per-task handoff journal is independent and runs either way.
 *
 * Everything else this capability set used to carry — the hosted vs. local
 * deployment flag, the constrained single-repo mode flag, automations, and the
 * three spend-hiding flags (token/token-usage/cost metrics) — is retired (A15,
 * decisions 5/7/8): there is no hosted deployment, no constrained single-repo
 * mode, no automations subsystem, and spend always renders. See spec §16a and
 * `BACKWARD_COMPATIBILITY.md`.
 */

import { followupsEnabled } from '../handoff.ts';
import type { Capabilities } from '@open-mercato/cezar-contract';

/** Every IPv4 address in 127.0.0.0/8, anchored. Anchoring is load-bearing: a
 *  `startsWith('127.')` test also matches attacker-controlled *hostnames* like
 *  `127.0.0.1.evil.com`, which is exactly the DNS-rebinding bypass #426 is
 *  about — see `isLoopbackHostHeader`. */
const LOOPBACK_V4 = /^127(?:\.(?:25[0-5]|2[0-4]\d|1?\d?\d)){3}$/;

/** The IPv6 loopback in the canonical form `expandIpv6` emits. */
const LOOPBACK_V6 = '0:0:0:0:0:0:0:1';

/** Expand an IPv6 literal to all 8 groups, lowercase and without leading zeros
 *  (`::1` → `0:0:0:0:0:0:0:1`), or `null` when it is not a valid literal.
 *
 *  Canonicalizing rather than merely *recognizing* matters: `Host` carries
 *  whatever spelling the client sent, but `new URL().hostname` compresses
 *  (`[0:0:0:0:0:0:0:1]` → `[::1]`), so the two only compare equal on a common
 *  form. */
function expandIpv6(h: string): string | null {
  const halves = h.split('::');
  if (halves.length > 2) return null;
  let groups: string[];
  if (halves.length === 2) {
    const head = halves[0] ? halves[0].split(':') : [];
    const tail = halves[1] ? halves[1].split(':') : [];
    if (head.length + tail.length > 7) return null;
    groups = [...head, ...Array(8 - head.length - tail.length).fill('0'), ...tail];
  } else {
    groups = h.split(':');
  }
  if (groups.length !== 8 || !groups.every((g) => /^[0-9a-f]{1,4}$/.test(g))) return null;
  return groups.map((g) => parseInt(g, 16).toString(16)).join(':');
}

/** Canonical hostname of an authority: port and IPv6 brackets removed, IPv6
 *  expanded, zone id and FQDN trailing dot dropped, lowercased.
 *  `[::1]:4321` → `0:0:0:0:0:0:0:1`, `localhost.:4321` → `localhost`.
 *
 *  Strict by construction: an authority that does not parse as one of the three
 *  legal shapes returns `''`, which no allowlist matches. Being lax here is a
 *  security bug, not a convenience — a parser that merely *finds* a loopback
 *  name inside the header would accept `[::1]@evil.com` or `127.0.0.1:evil.com`. */
export function normalizeHostname(host: string): string {
  // 1. Bracketed IPv6, optionally with a port: `[::1]`, `[::1%25eth0]:4321`.
  //    The end anchor is load-bearing — without it `[::1]@evil.com` → `::1`.
  const bracketed = host.match(/^\[([^\]]+)\](?::\d+)?$/);
  if (bracketed) return expandIpv6(stripZone(bracketed[1]!.toLowerCase())) ?? '';

  // 2. A bare IPv6 literal cannot carry a port (no brackets ⇒ every colon
  //    belongs to the address), so anything with >1 colon is taken whole.
  if (host.split(':').length > 2) return expandIpv6(stripZone(host.toLowerCase())) ?? '';

  // 3. `name` or `name:port`, where the port must actually be digits.
  const plain = host.match(/^([^:[\]]*)(?::(\d+))?$/);
  if (plain) return plain[1]!.toLowerCase().replace(/\.$/, '');

  return ''; // unparseable ⇒ matches no allowlist
}

/** Drop an IPv6 zone id (`fe80::1%eth0`), anchored to the end so a `%` inside a
 *  registrable hostname can never truncate it down to a loopback name. */
function stripZone(h: string): string {
  return h.replace(/%[a-z0-9._~-]+$/, '');
}

/** True when the hostname names this machine and nothing else. Expects the
 *  canonical output of `normalizeHostname`. */
function isLoopbackName(hostname: string): boolean {
  return hostname === 'localhost' || LOOPBACK_V4.test(hostname) || hostname === LOOPBACK_V6;
}

/** True for bind hosts that only the local machine can reach. Undefined = the
 *  default bind (127.0.0.1), hence trusted — this is a *configuration* value we
 *  chose, not a request header. Do NOT use this on attacker-controlled input;
 *  use `isLoopbackHostHeader`, which fails closed on a missing host. */
export function isLoopbackHost(host: string | undefined): boolean {
  if (!host) return true;
  return isLoopbackName(normalizeHostname(host));
}

/** True for a request's `Host`/`Origin` hostname when it names this machine.
 *  The untrusted-input twin of `isLoopbackHost`: a missing or unparseable host
 *  is **untrusted** (false), because absent is not the same as "we defaulted to
 *  loopback" once the value arrives over the wire (#426). */
export function isLoopbackHostHeader(host: string | null | undefined): boolean {
  if (!host) return false;
  return isLoopbackName(normalizeHostname(host));
}

/** `CEZ_FOLLOWUPS=1` ⇒ the follow-up inbox exists (#471).
 *
 *  Read per request — cheap. `followups` is honest per request too, but flipping it ON at
 *  runtime is only half a switch: the per-dataDir todos watch (step 2.3) is created by an SSE
 *  subscription, so connections opened while the flag was off never subscribed and get no live
 *  inbox updates until they reconnect. Hence the UI's "set CEZ_FOLLOWUPS=1 and restart cezar"
 *  wording — treat it as a boot-time flag.
 *
 *  `bindHost` is unused now that the hosted-deployment distinction is gone, but the loopback
 *  helpers it fed (`isLoopbackHost`/`isLoopbackHostHeader`) stay exported below — the
 *  origin/WS guards still use them directly (a port is still open through Phase A and B). */
export function resolveCapabilities(env: NodeJS.ProcessEnv = process.env): Capabilities {
  return {
    // Deliberately not re-derived here: RunManager enforces the same predicate,
    // and two spellings of "is the inbox on" would eventually disagree.
    followups: followupsEnabled(env),
  };
}
