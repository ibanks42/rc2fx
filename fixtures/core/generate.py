#!/usr/bin/env python3
"""Writes fixtures/core: the inputs, their canonical encodings and cases.json.

Run it from anywhere (`python3 fixtures/core/generate.py`); it rewrites the
files next to it. The canonical files are written the way serde_json 1.0
(without float_roundtrip, with zmij float formatting) writes them, and numbers
are read the way it reads them (see serde_parse), so both decoders must agree
with these files byte for byte.
"""
import copy
import math
import os
import sys
from decimal import Decimal


OUT = sys.argv[1] if len(sys.argv) > 1 else os.path.dirname(os.path.abspath(__file__))
cases = []

# ---- serde_json number reading (no float_roundtrip) ---------------------

POW10 = [float("1e%d" % i) for i in range(309)]
U64 = 2**64 - 1
I32 = 2**31 - 1

def overflow(s, d, mx):
    return s >= mx // 10 and (s > mx // 10 or d > mx % 10)

class OutOfRange(Exception): pass

def from_parts(positive, sig, exp):
    f = float(sig)
    while True:
        a = abs(exp)
        if a <= 308:
            if exp >= 0:
                f *= POW10[a]
                if math.isinf(f): raise OutOfRange()
            else:
                f /= POW10[a]
            break
        if f == 0.0: break
        if exp >= 0: raise OutOfRange()
        f /= 1e308
        exp += 308
    return f if positive else -f

def serde_parse(text):
    """serde_json (no float_roundtrip) number: ('u64'|'i64'|'f64', value)."""
    i = 0
    positive = True
    if text[i] == '-': positive = False; i += 1
    def digit(j): return j < len(text) and text[j].isdigit()
    c = text[i]; i += 1
    sig = 0
    def tail(i, sig):
        if i < len(text) and text[i] == '.': return decimal(i, sig, 0)
        if i < len(text) and text[i] in 'eE': return exponent(i, sig, 0)
        if positive: return ('u64', sig)
        neg = -sig
        if sig == 0 or sig > 2**63: return ('f64', -float(sig))
        return ('i64', neg)
    def long_integer(i, sig):
        e = 0
        while digit(i): i += 1; e += 1
        if i < len(text) and text[i] == '.': return decimal(i, sig, e)
        if i < len(text) and text[i] in 'eE': return exponent(i, sig, e)
        return ('f64', from_parts(positive, sig, e))
    def decimal(i, sig, before):
        i += 1
        after = 0
        while digit(i):
            d = int(text[i])
            if overflow(sig, d, U64):
                e = before + after
                while digit(i): i += 1
                if i < len(text) and text[i] in 'eE': return exponent(i, sig, e)
                return ('f64', from_parts(positive, sig, e))
            i += 1; sig = sig * 10 + d; after -= 1
        assert after != 0
        e = before + after
        if i < len(text) and text[i] in 'eE': return exponent(i, sig, e)
        return ('f64', from_parts(positive, sig, e))
    def exponent(i, sig, start):
        i += 1
        pos = True
        if text[i] == '+': i += 1
        elif text[i] == '-': pos = False; i += 1
        e = int(text[i]); i += 1
        while digit(i):
            d = int(text[i]); i += 1
            if overflow(e, d, I32):
                if sig != 0 and pos: raise OutOfRange()
                while digit(i): i += 1
                return ('f64', 0.0 if positive else -0.0)
            e = e * 10 + d
        final = start + e if pos else start - e
        final = max(-2**31, min(I32, final))
        return ('f64', from_parts(positive, sig, final))
    if c == '0':
        return tail(i, 0)
    sig = int(c)
    while digit(i):
        d = int(text[i])
        if overflow(sig, d, U64): return long_integer(i, sig)
        i += 1; sig = sig * 10 + d
    return tail(i, sig)



def fmt_float(x):
    if x == 0.0:
        return "-0.0" if math.copysign(1.0, x) < 0 else "0.0"
    neg = x < 0
    _, digits, exp = Decimal(repr(abs(x))).as_tuple()
    digits = list(digits)
    while len(digits) > 1 and digits[-1] == 0:
        digits.pop()
        exp += 1
    s = "".join(map(str, digits))
    n = len(s)
    e = n - 1 + exp
    if -5 <= e <= 15:
        if n - 1 <= e:
            out = s + "0" * (e - (n - 1)) + ".0"
        elif e >= 0:
            out = s[: e + 1] + "." + s[e + 1 :]
        else:
            out = "0." + "0" * (-e - 1) + s
    else:
        out = s[0] + ("." + s[1:] if n > 1 else "") + "e" + ("+" if e >= 0 else "-") + str(abs(e))
    return ("-" if neg else "") + out


def fmt_str(s):
    out = ['"']
    for ch in s:
        o = ord(ch)
        if ch == '"':
            out.append('\\"')
        elif ch == "\\":
            out.append("\\\\")
        elif o < 0x20:
            out.append({8: "\\b", 9: "\\t", 10: "\\n", 12: "\\f", 13: "\\r"}.get(o, "\\u%04x" % o))
        else:
            out.append(ch)
    out.append('"')
    return "".join(out)


def ser(v, pretty=False, depth=0):
    ind = lambda d: "\n" + "  " * d
    if v is None:
        return "null"
    if v is True:
        return "true"
    if v is False:
        return "false"
    if isinstance(v, int):
        return str(v)
    if isinstance(v, float):
        return fmt_float(v)
    if isinstance(v, str):
        return fmt_str(v)
    if isinstance(v, list):
        if not v:
            return "[]"
        if not pretty:
            return "[" + ",".join(ser(x) for x in v) + "]"
        return "[" + ",".join(ind(depth + 1) + ser(x, True, depth + 1) for x in v) + ind(depth) + "]"
    if isinstance(v, dict):
        if not v:
            return "{}"
        if not pretty:
            return "{" + ",".join(fmt_str(k) + ":" + ser(x) for k, x in v.items()) + "}"
        return (
            "{"
            + ",".join(ind(depth + 1) + fmt_str(k) + ": " + ser(x, True, depth + 1) for k, x in v.items())
            + ind(depth)
            + "}"
        )
    raise TypeError(v)


def write(name, data):
    path = os.path.join(OUT, name)
    os.makedirs(os.path.dirname(path), exist_ok=True)
    if isinstance(data, str):
        data = data.encode("utf-8")
    with open(path, "wb") as f:
        f.write(data)


def case(name, kind, file, data, accept, why, canonical=None, pretty=None, error=None):
    write(file, data)
    entry = {"name": name, "type": kind, "file": file, "accept": accept}
    if error is not None:
        entry["error"] = error
    if canonical is not None:
        cname, ctext = canonical
        if ctext is not None:
            write(cname, ctext)
        entry["canonical"] = cname
    if pretty is not None:
        pname, ptext = pretty
        if ptext is not None:
            write(pname, ptext)
        entry["pretty"] = pname
    entry["why"] = why
    cases.append(entry)


# ---- Config -----------------------------------------------------------------

GATE0 = {"strength": 0.0, "advanced": False, "threshold": 0.3, "reduction": 0.0, "attack": 5.0, "hold": 350.0, "release": 300.0}


def target(id, name, kind, channels):
    return {
        "id": id,
        "name": name,
        "kind": kind,
        "channels": channels,
        "bypass": False,
        "order": ["highpass", "suppression", "eq", "compressor", "gain", "limiter"],
        "highpass": {"enabled": False, "frequency": 80.0},
        "suppression": {"enabled": False, "wet": 1.0, "lookahead": 0, "gate": dict(GATE0)},
        "eq": {"enabled": False, "inputGainDb": 0.0, "outputGainDb": 0.0, "bands": []},
        "compressor": {"enabled": False, "threshold": -18.0, "ratio": 3.0, "attack": 10.0, "release": 120.0, "makeup": 0.0},
        "gain": {"enabled": False, "db": 0.0},
        "limiter": {"enabled": False, "ceiling": -1.0, "release": 100.0},
    }


def default_config():
    t = [target("mic-1", "Mic 4", "mic", 1)]
    t += [target(f"analog-{i}", f"Mic {i}", "mic", 1) for i in range(1, 4)]
    t += [target(f"usb-{n}", f"USB {n}", "playback", 2) for n in ["main", "chat", "secondary", "game", "music", "a", "b"]]
    t += [target(f"headphones-{i}", f"Headphones {i}", "output", 2) for i in range(1, 5)]
    t += [
        target(i, n, "output", 2)
        for i, n in [("monitors", "Monitors"), ("send-main", "Main Mix Out"), ("send-chat", "Chat Mix Out"), ("send-secondary", "Secondary Mix Out")]
    ]
    t += [target(f"callme-{i}", f"CallMe {i}", "playback", 2) for i in range(1, 4)]
    t += [target(f"wireless-{i}", f"Wireless {i}", "mic", 1) for i in range(1, 3)]
    t += [target("bluetooth", "Bluetooth", "playback", 2)]
    assert len(t) == 25
    return {"schemaVersion": 9, "revision": 1, "selectedTargets": [], "targets": t}


def idx(config, id):
    return next(i for i, t in enumerate(config["targets"]) if t["id"] == id)


D = default_config()
DEFAULT = ser(D)
case("default config", "config", "config/default.json", DEFAULT, True,
     "Config::default() exactly as serde_json::to_string writes it",
     canonical=("config/default.json", None), pretty=("config/default-pretty.json", ser(D, True)))
case("default config, pretty preset file", "config", "config/default-pretty.json", ser(D, True), True,
     "a preset file as the app saves it (to_string_pretty)",
     canonical=("config/default.json", None), pretty=("config/default-pretty.json", None))

legacy = copy.deepcopy(D)
del legacy["targets"][0]["suppression"]["lookahead"]
del legacy["targets"][5]["suppression"]["lookahead"]
case("lookahead absent means 0", "config", "config/legacy-no-lookahead.json", ser(legacy), True,
     "settings saved before the lookahead field existed",
     canonical=("config/default.json", None))

# An edited configuration, written in a style serde never writes: integers
# for floats, members out of order, whitespace. The canonical form fixes all.
E = copy.deepcopy(D)
E["revision"] = 42
E["selectedTargets"] = ["mic-1", "send-chat"]
mic = E["targets"][0]
mic["suppression"] = {"enabled": True, "wet": 0.85, "lookahead": 1, "gate": {
    "strength": 0.7, "advanced": True, "threshold": 0.45, "reduction": 18.5, "attack": 2.5, "hold": 120.0, "release": 250.0}}
mic["highpass"] = {"enabled": True, "frequency": 95.5}
mic["eq"] = {"enabled": True, "inputGainDb": -1.5, "outputGainDb": 0.25, "bands": [
    {"id": "band-1", "enabled": True, "type": "bell", "mode": "rlc-bt", "solo": False, "frequency": 120.0, "gain": 3.5, "q": 1.2},
    {"id": "band-2", "enabled": False, "type": "high-shelf", "mode": "apo-dr", "solo": True, "frequency": 8000.0, "gain": -2.25, "q": 0.7071067811865476},
    {"id": "band-10", "enabled": True, "type": "notch", "mode": "apo-dr", "solo": False, "frequency": 23999.999, "gain": 0.0, "q": 100.0},
]}
mic["compressor"] = {"enabled": True, "threshold": -24.0, "ratio": 4.5, "attack": 0.1, "release": 2000.0, "makeup": 6.0}
chat = E["targets"][idx(E, "send-chat")]
chat["name"] = "Chat \u00b7 \"voice\" \\ mix"
chat["eq"] = {"enabled": True, "inputGainDb": 0.0, "outputGainDb": 0.0, "bands": [], "rightBands": [
    {"id": "band-1", "enabled": True, "type": "low-pass", "mode": "apo-dr", "solo": False, "frequency": 18000.0, "gain": 0.0, "q": 0.5}]}
chat["gain"] = {"enabled": True, "db": -3.0}
chat["limiter"] = {"enabled": True, "ceiling": -0.1, "release": 10.0}
E_CANON = ser(E)

messy = copy.deepcopy(E)


def ints(v):
    """Integral floats as integer literals, as a hand-edited file might have."""
    if isinstance(v, float) and v.is_integer() and not (v == 0 and math.copysign(1, v) < 0):
        return int(v)
    if isinstance(v, list):
        return [ints(x) for x in v]
    if isinstance(v, dict):
        return {k: ints(x) for k, x in reversed(list(v.items()))}
    return v


messy = ints(messy)
messy_text = ser(messy, True).replace("\n", "\r\n\t")
case("edited config, hand-written style", "config", "config/edited.json", messy_text, True,
     "integers for floats, members in reverse order, CRLF and tabs; rightBands present, an escaped name",
     canonical=("config/edited.canonical.json", E_CANON))


# Rejections and device states use a four-path catalog to keep the files
# small; the defaults above cover the full one.
SMALL = copy.deepcopy(D)
SMALL["targets"] = [copy.deepcopy(D["targets"][idx(D, i)]) for i in ("mic-1", "analog-1", "usb-main", "send-chat")]
SMALL_TEXT = ser(SMALL)


def mutate(fn):
    c = copy.deepcopy(SMALL)
    fn(c)
    return ser(c)


def set_path(c, path, value):
    node = c
    for p in path[:-1]:
        node = node[p]
    node[path[-1]] = value


def bad(name, why, text, error=None):
    case(name, "config", f"config/reject-{name}.json", text, False, why, error=error)


SCHEMA_ERROR = "Expected schema version 9 and a positive revision"


bad("unknown-top-level-field", "deny_unknown_fields", mutate(lambda c: c.update(screenConversion="retired")))
bad("retired-suppression-field", "removed suppression options",
    mutate(lambda c: c["targets"][0]["suppression"].update(algorithm="retired")))
bad("missing-selected-targets", "schema 5+ requires a selection", mutate(lambda c: c.pop("selectedTargets")))
bad("missing-gate", "required field", mutate(lambda c: c["targets"][3]["suppression"].pop("gate")))
for v in [8, 10]:
    bad(f"schema-{v}", "only the current schema", mutate(lambda c, v=v: c.update(schemaVersion=v)), SCHEMA_ERROR)
bad("revision-zero", "a positive revision", mutate(lambda c: c.update(revision=0)), SCHEMA_ERROR)
bad("revision-negative", "u64 out of range", mutate(lambda c: c.update(revision=-1)))
bad("revision-float", "integer fields need integer literals", mutate(lambda c: c.update(revision=1.0)))
bad("revision-overflow", "beyond u64 is a float", SMALL_TEXT.replace('"revision":1,', '"revision":18446744073709551616,', 1))
bad("channels-256", "u8 out of range", mutate(lambda c: set_path(c, ["targets", 0, "channels"], 256)))
bad("channels-float", "integer fields need integer literals", mutate(lambda c: set_path(c, ["targets", 0, "channels"], 1.5)))
bad("channels-3", "1 or 2 channels", mutate(lambda c: set_path(c, ["targets", 0, "channels"], 3)))
bad("null-right-bands", "rightBands may be absent but not null", mutate(lambda c: set_path(c, ["targets", 0, "eq", "rightBands"], None)))
bad("right-bands-on-mono", "independent right EQ needs a stereo target", mutate(lambda c: set_path(c, ["targets", 0, "eq", "rightBands"], [])))
bad("null-bands", "a required list", mutate(lambda c: set_path(c, ["targets", 0, "eq", "bands"], None)))
bad("null-lookahead", "a defaulted field is not optional", mutate(lambda c: set_path(c, ["targets", 0, "suppression", "lookahead"], None)))
bad("duplicate-key", "duplicate keys at the top", SMALL_TEXT.replace('"schemaVersion":9', '"schemaVersion":9,"schemaVersion":9', 1))
bad("duplicate-nested-key", "duplicate keys at depth",
    SMALL_TEXT.replace('"frequency":80.0', '"frequency":80.0,"frequency":90.0', 1))
bad("duplicate-escaped-key", "an escaped spelling is still a duplicate",
    SMALL_TEXT.replace('"revision":1', '"revision":1,"\\u0072evision":1', 1))
bad("trailing-data", "one value only", SMALL_TEXT + " {}")
bad("two-suppressed-targets", "suppression on one target at a time",
    mutate(lambda c: [set_path(c, ["targets", i, "suppression", "enabled"], True) for i in (0, 1)]))
bad("suppression-on-playback", "suppression on a mono mic or the Chat mix only",
    mutate(lambda c: set_path(c, ["targets", 2, "suppression", "enabled"], True)))
bad("unknown-kind", "unknown enum variant", mutate(lambda c: set_path(c, ["targets", 0, "kind"], "speaker")))
bad("unknown-band-type", "unknown enum variant", mutate(lambda c: set_path(c, ["targets", 0, "eq", "bands"], [
    {"id": "band-1", "enabled": True, "type": "peak", "mode": "apo-dr", "solo": False, "frequency": 1000.0, "gain": 0.0, "q": 1.0}])))
bad("band-id", "band ids are lowercase ASCII", mutate(lambda c: set_path(c, ["targets", 0, "eq", "bands"], [
    {"id": "Band 1", "enabled": True, "type": "bell", "mode": "apo-dr", "solo": False, "frequency": 1000.0, "gain": 0.0, "q": 1.0}])))
bad("duplicate-band-id", "band ids are unique per channel", mutate(lambda c: set_path(c, ["targets", 0, "eq", "bands"], [
    {"id": "band-1", "enabled": True, "type": "bell", "mode": "apo-dr", "solo": False, "frequency": 1000.0, "gain": 0.0, "q": 1.0}] * 2)))
bad("band-frequency", "frequency within 10..24000", mutate(lambda c: set_path(c, ["targets", 0, "eq", "bands"], [
    {"id": "band-1", "enabled": True, "type": "bell", "mode": "apo-dr", "solo": False, "frequency": 24000.5, "gain": 0.0, "q": 1.0}])))
bad("lookahead-2", "lookahead 0 or 1", mutate(lambda c: set_path(c, ["targets", 0, "suppression", "lookahead"], 2)))
bad("gate-strength", "gate strength within 0..1", mutate(lambda c: set_path(c, ["targets", 0, "suppression", "gate", "strength"], 1.5)))
bad("gate-retired-shape", "the gate's old fields", mutate(lambda c: set_path(c, ["targets", 0, "suppression", "gate"], {"enabled": True, "strength": 0.5})))
bad("gain-13", "gain within -60..12", mutate(lambda c: set_path(c, ["targets", 0, "gain", "db"], 13)))
bad("duplicate-target-id", "target ids are unique", mutate(lambda c: set_path(c, ["targets", 1, "id"], "mic-1")))
bad("duplicate-stage", "each stage once", mutate(lambda c: set_path(c, ["targets", 0, "order", 1], "eq")))
bad("unknown-selection", "selection names known targets", mutate(lambda c: c.update(selectedTargets=["nowhere"])))
bad("duplicate-selection", "selection is unique", mutate(lambda c: c.update(selectedTargets=["mic-1", "mic-1"])))
bad("no-targets", "1 to 32 targets", mutate(lambda c: c.update(targets=[])), "Expected 1 to 32 targets")
bad("33-targets", "1 to 32 targets", mutate(lambda c: c.update(targets=[
    dict(copy.deepcopy(c["targets"][0]), id=f"mic-{i}") for i in range(1, 34)])), "Expected 1 to 32 targets")
bad("number-out-of-range", "1e400 is not a finite number", SMALL_TEXT.replace('"frequency":80.0', '"frequency":1e400', 1))
bad("byte-order-mark", "JSON text does not start with a BOM", "\ufeff" + SMALL_TEXT)
bad("raw-tab-in-string", "control characters must be escaped", SMALL_TEXT.replace('"Mic 4"', '"Mic\t4"', 1))
bad("not-utf8", "input must be UTF-8", SMALL_TEXT.encode().replace(b'"Mic 4"', b'"Mic \xff"', 1))
bad("lone-surrogate", "a lone surrogate escape", SMALL_TEXT.replace('"Mic 4"', '"Mic \\ud800"', 1))

# ---- DeviceState ------------------------------------------------------------


def device_state(config, engine, routing, selection):
    return {"config": config, "engine": engine, "routing": routing, "selection": selection}


def engine(**kw):
    e = {"generation": 1, "restartAllowed": False, "mode": "jack", "status": "running", "appliedRevision": 1,
         "sampleRate": 48000, "blockSize": 112, "message": "", "audioAdvancing": True}
    e.update(kw)
    return e


def routing(**kw):
    r = {"status": "active", "channels": 5, "recovered": 0, "message": ""}
    r.update(kw)
    return r


def selection(**kw):
    s = {"channelCapacity": None, "status": "idle", "allowed": True,
         "qualifiedTargetIds": ["mic-1", "usb-main", "send-chat"],
         "activeTargetIds": ["mic-1", "usb-main", "send-chat"], "pendingTargetIds": [], "message": ""}
    s.update(kw)
    return s


SEL = copy.deepcopy(SMALL)
SEL["selectedTargets"] = ["mic-1", "usb-main", "send-chat"]
JACK = device_state(SEL, engine(), routing(), selection())
case("jack engine running", "deviceState", "device-state/jack-running.json", ser(JACK), True,
     "a JACK mixer applying three paths", canonical=("device-state/jack-running.json", None))

native_config = copy.deepcopy(SMALL)
native_config["revision"] = 2
native_config["selectedTargets"] = ["mic-1"]
NATIVE = device_state(
    native_config,
    engine(generation=7, mode="native", status="starting", appliedRevision=0, audioAdvancing=False,
           message="Starting paths \u00e9\u00e8 / ok"),
    routing(status="starting", channels=3, message="Routing \"two\" paths \\ wait"),
    selection(channelCapacity=8, status="changing", allowed=False, activeTargetIds=["mic-1", "usb-main"],
              pendingTargetIds=["mic-1"], message="Changing paths"),
)
native_text = ser(NATIVE).replace("\u00e9\u00e8 / ok", "\\u00e9\u00e8 \\/ ok", 1)
case("native engine mid-change", "deviceState", "device-state/native-changing.json", native_text, True,
     "a native mixer committing a path change; escaped é and \\/ in the input come out raw",
     canonical=("device-state/native-changing.canonical.json", ser(NATIVE)))


def bad_state(name, why, fn=None, text=None):
    if text is None:
        s = copy.deepcopy(JACK)
        fn(s)
        text = ser(s)
    case(name, "deviceState", f"device-state/reject-{name}.json", text, False, why)


bad_state("missing-selection", "required field", lambda s: s.pop("selection"))
bad_state("missing-generation", "the restart wire needs a generation", lambda s: s["engine"].pop("generation"))
bad_state("missing-restart-allowed", "the restart wire needs explicit eligibility", lambda s: s["engine"].pop("restartAllowed"))
bad_state("unknown-engine-field", "deny_unknown_fields", lambda s: s["engine"].update(pid=7))
bad_state("message-escape", "status text with a control character", lambda s: s["engine"].update(message="escape\u001b[2J"))
bad_state("message-bidi", "status text with a bidi override", lambda s: s["routing"].update(message="reversed\u202etxt"))
bad_state("message-too-long", "status text at most 4096 bytes", lambda s: s["selection"].update(message="x" * 4097))
bad_state("capacity-129", "channel capacity at most 128", lambda s: s["selection"].update(channelCapacity=129))
bad_state("pending-unqualified", "pending paths must be qualified",
          lambda s: s["selection"].update(status="changing", allowed=False, pendingTargetIds=["analog-1"]))
bad_state("active-out-of-order", "ids in catalog order", lambda s: s["selection"].update(activeTargetIds=["send-chat", "mic-1"]))
bad_state("changing-allowed", "a pending change cannot allow another", lambda s: s["selection"].update(status="changing"))
bad_state("engine-mode", "unknown enum variant", lambda s: s["engine"].update(mode="pipewire"))
bad_state("invalid-config", "the embedded config is validated", lambda s: s["config"].update(schemaVersion=6))
bad_state("sample-rate-float", "integer fields need integer literals", lambda s: s["engine"].update(sampleRate=48000.0))

# ---- ConfigUpdate -----------------------------------------------------------

UPDATE = {"expectedRevision": 3, "targets": copy.deepcopy(SMALL["targets"])}
case("effects update", "configUpdate", "config-update/update.json", ser(UPDATE), True,
     "a PUT body", canonical=("config-update/update.json", None))
u = copy.deepcopy(UPDATE)
u["selectedTargets"] = ["mic-1"]
case("update carrying a selection", "configUpdate", "config-update/reject-selected-targets.json", ser(u), False,
     "effects saves never carry a selection")
u = copy.deepcopy(UPDATE)
del u["expectedRevision"]
case("update without a revision", "configUpdate", "config-update/reject-missing-revision.json", ser(u), False, "required field")

# ---- PresetSummary ----------------------------------------------------------

PRESETS = [{"id": "warm-voice", "name": "Warm voice"}, {"id": "a", "name": "\u00c4 \"quoted\" \\ name"}]
case("preset list", "presetSummaries", "presets/list.json", ser(PRESETS, True), True,
     "the mixer's preset list", canonical=("presets/list.canonical.json", ser(PRESETS)))
case("preset list, extra field", "presetSummaries", "presets/reject-extra-field.json",
     '[{"id":"a","name":"A","builtIn":true}]', False, "deny_unknown_fields")
case("preset list, missing name", "presetSummaries", "presets/reject-missing-name.json", '[{"id":"a"}]', False, "required field")
case("preset list, not a list", "presetSummaries", "presets/reject-object.json", '{"id":"a","name":"A"}', False, "a list")

# ---- MixerRelease -----------------------------------------------------------

REL = {"schemaVersion": 1, "version": "0.2.0", "protocolVersion": 1}
case("mixer release manifest", "mixerRelease", "mixer-release/release.json", ser(REL, True) + "\n", True,
     "config/mixer-release.json as shipped", canonical=("mixer-release/release.canonical.json", ser(REL)))
for name, text, why in [
    ("schema-2", '{"schemaVersion":2,"version":"0.1.0","protocolVersion":1}', "schema 1 only"),
    ("version-word", '{"schemaVersion":1,"version":"latest","protocolVersion":1}', "dotted numeric version"),
    ("version-prerelease", '{"schemaVersion":1,"version":"1.2.3-rc1","protocolVersion":1}', "dotted numeric version"),
    ("version-part-too-long", '{"schemaVersion":1,"version":"1.1234567890","protocolVersion":1}', "parts of at most 9 digits"),
    ("protocol-0", '{"schemaVersion":1,"version":"0.1.0","protocolVersion":0}', "a non-zero protocol"),
    ("protocol-overflow", '{"schemaVersion":1,"version":"0.1.0","protocolVersion":4294967296}', "u32 out of range"),
    ("unknown-field", '{"schemaVersion":1,"version":"0.1.0","protocolVersion":1,"x":1}', "deny_unknown_fields"),
    ("duplicate-version", '{"schemaVersion":1,"version":"0.1.0","version":"0.2.0","protocolVersion":1}', "duplicate key"),
    ("missing-protocol", '{"schemaVersion":1,"version":"0.1.0"}', "required field"),
    ("too-many-values", '{"schemaVersion":1,"version":"0.1.0","protocolVersion":1,"x":[1,2,3,4,5,6,7,8,9,10,11,12,13]}',
     "16 values at most (checked before fields)"),
]:
    case(f"mixer release, {name}", "mixerRelease", f"mixer-release/reject-{name}.json", text, False, why)
big = '{"schemaVersion":1,"version":"0.1.0","protocolVersion":1}'
case("mixer release, too large", "mixerRelease", "mixer-release/reject-too-large.json", big + " " * (4097 - len(big)), False, "4 KiB at most")
case("mixer release, not UTF-8", "mixerRelease", "mixer-release/reject-not-utf8.json",
     b'{"schemaVersion":1,"version":"0.1.0\xff","protocolVersion":1}', False, "the manifest is UTF-8")

# ---- DeviceVersions ---------------------------------------------------------

VERS = {"runtime": "0.2.0", "protocol": 2, "nativeProtocol": 6, "firmware": "1.7.7",
        "firmwareProfile": "rodecaster-pro-ii-1.7.7", "inPlaceUpdates": 2}
newer = dict(VERS)
newer["future"] = {"nested": [True]}
case("device versions from a newer mixer", "deviceVersions", "device-versions/newer.json", ser(newer), True,
     "unknown fields are ignored", canonical=("device-versions/newer.canonical.json", ser(VERS)))
case("device versions from an older mixer", "deviceVersions", "device-versions/empty.json", "{}", True,
     "every field optional", canonical=("device-versions/empty.json", None))
case("device versions with nulls", "deviceVersions", "device-versions/nulls.json", '{"runtime":null,"protocol":null}', True,
     "null is an absent Option", canonical=("device-versions/empty.json", None))
case("device versions, negative protocol", "deviceVersions", "device-versions/reject-negative.json", '{"protocol":-1}', False, "u32 out of range")
case("device versions, text with a newline", "deviceVersions", "device-versions/reject-newline.json",
     '{"firmware":"1.7.7\\nrm -rf"}', False, "version text fit to show")
case("device versions, duplicate key", "deviceVersions", "device-versions/reject-duplicate.json",
     '{"runtime":"0.1.0","runtime":"0.2.0"}', False, "duplicate key")

# ---- SshStatus --------------------------------------------------------------

KEY = "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIEPzD7gW9u5+SC5iq8gzujWEOTBQO1CbVCLO8fAOGkip"
SSH = {"managed": True, "managedFingerprint": "SHA256:QbAgxtEX0z3ZCUzJ86NO6a0PYcnvrkRmVh0+cMm05TY",
       "unmanagedKeys": 1, "hostKeys": [KEY], "serverRunning": True, "detail": "Running"}
ssh_newer = dict(SSH)
ssh_newer["future"] = 1
case("ssh status", "sshStatus", "ssh-status/status.json", ser(ssh_newer), True,
     "unknown fields are ignored", canonical=("ssh-status/status.canonical.json", ser(SSH)))
case("ssh status, minimal", "sshStatus", "ssh-status/minimal.json", '{"managed":false,"unmanagedKeys":0,"hostKeys":[]}', True,
     "optional fields absent", canonical=("ssh-status/minimal.json", None))
for name, text, why in [
    ("missing-host-keys", '{"managed":false,"unmanagedKeys":0}', "a reply missing what the status means"),
    ("missing-managed", '{"unmanagedKeys":0,"hostKeys":[]}', "a reply missing what the status means"),
    ("array", "[]", "not an object"),
    ("null", "null", "not an object"),
    ("host-key-newline", '{"managed":false,"unmanagedKeys":0,"hostKeys":["' + KEY + '\\nssh-ed25519 other"]}', "host keys fit to show"),
]:
    case(f"ssh status, {name}", "sshStatus", f"ssh-status/reject-{name}.json", text, False, why)

# ---- Meter ------------------------------------------------------------------


def timing(calls):
    return {"calls": calls, "totalNs": calls * 100, "maxNs": 200, "overruns": 1, "clockErrors": 0,
            "histogram": [1, 2, 3, 4, 5, 6, 7, 8]}


# Read as serde_json (without float_roundtrip) reads them: the long integer
# and the last value are not the correctly rounded doubles.
SPEC_TEXT = ["1e-7", "1.5E+300", "123456789012345678901", "1e-5", "-0", "1e16", "1e15", "123.456", "0.1", "5e-324",
             "1.7976931348623157e308", "100", "2.5e3", "-42", "0.3333333333333333", "9007199254740992", "1.5e-5",
             "9999999999999999", "1.2345678901234567e-300"]
SPECTRUM = [float(serde_parse(t)[1]) for t in SPEC_TEXT]
assert SPECTRUM[2] == 1.2345678901234567e20 and SPECTRUM[-1] == 1.234567890123457e-300
TARGET = {"id": "mic-1", "inputPeak": 0.5, "outputPeak": 0.25, "rms": 0.125, "gainReduction": 3.0, "voice": 0.75,
          "gateOpen": 1.0, "clipped": 2, "spectrum": SPECTRUM, "callbacks": 9, "processedFrames": 900,
          "suppressionFrames": 9, "missedDeadlines": 0, "staleResults": 0, "inputDrops": 1, "outputDrops": 0,
          "latencySamples": 808, "workerFailed": False,
          "performance": {"callbackBudgetNs": 2333333, "workerBudgetNs": 9333333, "xruns": 3,
                          "callback": timing(9), "workerWall": timing(4), "workerCpu": timing(4),
                          "callbackRecentMaxNs": 180}}
METER = {"type": "meter", "revision": 7, "generation": 8, "synthetic": False, "diagnosticsEpoch": 3,
         "runningForNs": 123456789, "targets": [TARGET]}
meter_in = copy.deepcopy(METER)
meter_in["future"] = "ignored"
meter_in["targets"][0]["performance"]["futureCounter"] = 5
spectrum_text = "[" + ",".join(SPEC_TEXT) + "]"
meter_text = ser(meter_in).replace(ser(SPECTRUM), spectrum_text, 1)
assert spectrum_text in meter_text
case("meter with performance", "meter", "meter/performance.json", meter_text, True,
     "float formatting: exponents, -0, subnormals, integers as floats; unknown meter and performance fields ignored",
     canonical=("meter/performance.canonical.json", ser(METER)))
MIN = {"type": "meter", "revision": 1, "generation": 1, "synthetic": True, "targets": []}
MIN_CANON = {"type": "meter", "revision": 1, "generation": 1, "synthetic": True, "diagnosticsEpoch": 0, "targets": []}
case("meter from an older mixer", "meter", "meter/minimal.json", ser(MIN), True,
     "no epoch reads as 0", canonical=("meter/minimal.canonical.json", ser(MIN_CANON)))
for name, fn, why in [
    ("missing-generation", lambda m: m.pop("generation"), "required field"),
    ("type", lambda m: m.update(type="meters"), "unknown enum variant"),
    ("histogram-length", lambda m: m["targets"][0]["performance"]["callback"].update(histogram=[1, 2, 3, 4, 5, 6, 7]), "exactly 8 buckets"),
    ("histogram-long", lambda m: m["targets"][0]["performance"]["callback"].update(histogram=list(range(9))), "exactly 8 buckets"),
    ("target-unknown-field", lambda m: m["targets"][0].update(extra=1), "target meters deny unknown fields"),
    ("target-missing-worker-failed", lambda m: m["targets"][0].pop("workerFailed"), "required field"),
]:
    m = copy.deepcopy(METER)
    fn(m)
    case(f"meter, {name}", "meter", f"meter/reject-{name}.json", ser(m), False, why)

# ---- DiagnosticsEvents ------------------------------------------------------

PAGE = {"nowNs": 10, "nextSeq": 4, "lost": 1, "more": False, "events": [
    {"seq": 1, "kind": "xrun", "atNs": 5, "message": "JACK xrun", "details": {"valueNs": 1120, "delayedUs": 240}},
    {"seq": 2, "kind": "outputDrops", "path": "mic-1", "atNs": 6, "message": "3 output drops on mic-1",
     "details": {"delta": 3, "intervalNs": 20000000}},
    {"seq": 3, "kind": "reset", "atNs": 7, "message": "Diagnostics reset"},
]}
page_in = copy.deepcopy(PAGE)
page_in["events"][2]["path"] = None
page_in["events"][2]["details"] = {}
case("diagnostics page", "diagnosticsEvents", "diagnostics/page.json", ser(page_in), True,
     "a null path and empty details are left out when written",
     canonical=("diagnostics/page.canonical.json", ser(PAGE)))
unknown_kind = copy.deepcopy(PAGE)
unknown_kind["events"][0]["kind"] = "newKind"
unknown_kind_canon = copy.deepcopy(PAGE)
unknown_kind_canon["events"][0]["kind"] = "other"
case("diagnostics page, unknown-kind", "diagnosticsEvents", "diagnostics/unknown-kind.json", ser(unknown_kind), True,
     "a newer mixer's unknown event kind arrives as other",
     canonical=("diagnostics/unknown-kind.canonical.json", ser(unknown_kind_canon)))
for name, fn, why in [
    ("details-unknown-field", lambda p: p["events"][0]["details"].update(extra=1), "deny_unknown_fields"),
    ("page-unknown-field", lambda p: p.update(extra=1), "deny_unknown_fields"),
]:
    p = copy.deepcopy(PAGE)
    fn(p)
    case(f"diagnostics page, {name}", "diagnosticsEvents", f"diagnostics/reject-{name}.json", ser(p), False, why)

# ---- StockInventory ---------------------------------------------------------

SHOW = {"available": True, "showName": "Show", "revision": "ab" * 32, "message": "", "inputs": [
    {"id": "input-3", "sourceId": 3, "fader": 1, "name": "Mic 4", "color": "fffdba0e", "targetId": "mic-1"},
    {"id": "input-7", "sourceId": 7, "fader": None, "name": "Guest", "color": "FF00AA11", "targetId": ""},
]}
case("show inventory", "stockInventory", "inventory/show.json", ser(SHOW), True, "a saved show",
     canonical=("inventory/show.json", None))
for name, fn, why in [
    ("duplicate-fader", lambda s: s["inputs"][1].update(fader=1), "a fader is assigned once"),
    ("source-overflow", lambda s: s["inputs"][1].update(sourceId=65536, id="input-65536"), "u16 out of range"),
    ("source-256", lambda s: s["inputs"][1].update(sourceId=256, id="input-256"), "sources 0 to 255"),
    ("unavailable-with-inputs", lambda s: s.update(available=False), "an unavailable show has no inputs"),
]:
    s = copy.deepcopy(SHOW)
    fn(s)
    case(f"show inventory, {name}", "stockInventory", f"inventory/reject-{name}.json", ser(s), False, why)

# ---- Untyped JSON with the default limits -----------------------------------

deep64 = "[" * 64 + "0" + "]" * 64
deep65 = "[" * 65 + "0" + "]" * 65
for name, text, accept, canonical, why in [
    ("depth-64", deep64, True, deep64, "nesting to depth 64 is accepted"),
    ("depth-65", deep65, False, None, "nesting beyond depth 64"),
    ("negative-zero", "-0", True, "-0.0", "-0 is the float -0.0"),
    ("u64-max", "18446744073709551615", True, "18446744073709551615", "the largest unsigned integer"),
    ("beyond-u64", "18446744073709551616", True, "1.8446744073709552e+19", "a larger integer is a float"),
    ("i64-min", "-9223372036854775808", True, "-9223372036854775808", "the smallest signed integer"),
    ("beyond-i64", "-9223372036854775809", True, "-9.223372036854776e+18", "a smaller integer is a float"),
    ("exponent", "1E2", True, "100.0", "an exponent makes a float"),
    ("tiny", "1e-400", True, "0.0", "underflow rounds to zero"),
    ("long-integer", "123456789012345678901", True, "1.2345678901234567e+20",
     "serde_json keeps 20 digits and scales by ten, not the correctly rounded 1.2345678901234568e20"),
    ("inexact-float", "1.2345678901234567e-300", True, "1.234567890123457e-300",
     "serde_json divides by a rounded power of ten, not the correctly rounded 1.2345678901234568e-300"),
    ("many-digits", "3.141592653589793238462643383279", True, "3.141592653589793", "digits past 19 are ignored"),
    ("huge-exponent-zero", "0e99999999999", True, "0.0", "a zero significand with an overflowing exponent"),
    ("huge-negative-exponent", "1e-99999999999", True, "0.0", "an overflowing negative exponent is zero"),
    ("huge-exponent", "1e99999999999", False, None, "an overflowing positive exponent is out of range"),
    ("surrogate-pair", '"\\ud83d\\ude00"', True, '"\U0001F600"', "an escaped pair is one character"),
    ("escapes", '"\\b\\f\\n\\r\\t\\u0001\\u001f\\u007f\\/\\u2028<>&"', True,
     '"\\b\\f\\n\\r\\t\\u0001\\u001f\x7f/\u2028<>&"', "serde_json escapes only quotes, backslashes and control characters"),
    ("object", '{ "a" : [ true , false , null ] }', True, '{"a":[true,false,null]}', "whitespace between tokens"),
    ("number-out-of-range", "1e400", False, None, "not a finite number"),
    ("lone-surrogate", '"\\ud800"', False, None, "a lone surrogate"),
    ("lone-low-surrogate", '"\\udc00"', False, None, "a lone low surrogate"),
    ("leading-zero", "01", False, None, "no leading zeros"),
    ("bare-point", "1.", False, None, "digits after the point"),
    ("plus-sign", "+1", False, None, "no plus sign"),
    ("trailing-comma", "[1,]", False, None, "no trailing comma"),
    ("trailing-comma-object", '{"a":1,}', False, None, "no trailing comma"),
    ("nan", "NaN", False, None, "no NaN"),
    ("infinity", "Infinity", False, None, "no Infinity"),
    ("raw-newline", '"a\nb"', False, None, "control characters must be escaped"),
    ("single-quotes", "'a'", False, None, "strings use double quotes"),
    ("comment", "[1] // note", False, None, "no comments"),
    ("empty", "", False, None, "a value is required"),
    ("whitespace", " \t\r\n", False, None, "a value is required"),
    ("two-values", "[] []", False, None, "one value only"),
    ("unquoted-key", "{a:1}", False, None, "keys are strings"),
    ("bad-escape", '"\\x41"', False, None, "only JSON escapes"),
    ("duplicate-nested", '{"a":{"x":1,"x":2}}', False, None, "duplicate keys at depth"),
    ("duplicate-escaped", '{"x":1,"\\u0078":2}', False, None, "an escaped spelling is still a duplicate"),
]:
    file = f"json/{'' if accept else 'reject-'}{name}.json"
    case(f"json {name}", "json", file, text, accept, why,
         canonical=(f"json/{name}.canonical.json", canonical) if accept else None)
case("json not UTF-8", "json", "json/reject-not-utf8.json", b'"\xc3\x28"', False, "input must be UTF-8")

manifest = {"comment": "Shared accept/reject fixtures for rc2fx-core. The Go port (desktop/core, shared_fixtures_test.go) "
            "and the Rust crate (crates/rc2fx-core/tests/shared_fixtures.rs) read the same files; each checks its own direction. "
            "For accepted cases the decoded value, written back, must equal `canonical` (compact) "
            "and `pretty` (to_string_pretty) byte for byte. The Go port reaches every verdict; the Rust crate skips the reject cases "
            "of documents only the desktop reads (deviceState, deviceVersions, sshStatus, meter, diagnosticsEvents, presetSummaries). "
            "A rejected case with `error` must fail with exactly that message on each side that reads it. generate.py writes this directory: change it and run it rather than editing files by hand. .gitattributes keeps every byte as committed (CRLF and odd bytes are part of some cases).",
            "cases": cases}
write("cases.json", ser(manifest, True) + "\n")
print(len(cases), "cases")
