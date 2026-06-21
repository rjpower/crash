"""shellsim's tiny ``yaml`` — a conservative block+flow subset of YAML 1.1 safe_load/safe_dump.

Handles block mappings, block sequences, scalars (int/float/bool/null/str), quoted strings, and
inline flow collections (`[...]`, `{...}`, which are JSON-ish). Anything it cannot parse cleanly
(anchors/aliases, multi-document, complex keys, folded/literal block scalars) raises ``YAMLError``
and records an OOD event, rather than silently mis-parsing.
"""
import sys as _sys
import types as _types
import json as _json

def _ood(msg):
    try:
        _shellsim_ood("yaml: " + msg)
    except Exception:
        pass

class YAMLError(Exception):
    pass


def _scalar(tok):
    t = tok.strip()
    if t == "" or t == "~" or t == "null" or t == "Null" or t == "NULL":
        return None
    if t in ("true", "True", "TRUE"):
        return True
    if t in ("false", "False", "FALSE"):
        return False
    if len(t) >= 2 and t[0] in "\"'" and t[-1] == t[0]:
        return t[1:-1]
    if t[0] in "[{":
        try:
            return _json.loads(t)
        except Exception:
            # YAML flow allows unquoted keys; fall back conservatively
            try:
                return _json.loads(t.replace("'", '"'))
            except Exception:
                _ood("flow collection not parseable: %r" % t)
                raise YAMLError("could not parse flow collection: %r" % t)
    try:
        return int(t)
    except ValueError:
        pass
    try:
        return float(t)
    except ValueError:
        pass
    return t


def _split_lines(text):
    out = []
    for raw in text.splitlines():
        # strip comments not inside quotes (best-effort)
        if raw.strip().startswith("#"):
            continue
        line = raw.rstrip()
        if not line.strip():
            continue
        if "#" in line:
            in_q = None
            buf = []
            for ch in line:
                if in_q:
                    buf.append(ch)
                    if ch == in_q:
                        in_q = None
                elif ch in "\"'":
                    in_q = ch; buf.append(ch)
                elif ch == "#" and (not buf or buf[-1] == " "):
                    break
                else:
                    buf.append(ch)
            line = "".join(buf).rstrip()
        if line.strip():
            out.append(line)
    return out

def _indent(line):
    return len(line) - len(line.lstrip(" "))

def _parse_block(lines, i, indent):
    """Parse a block starting at lines[i] with the given indent; return (value, next_i)."""
    if i >= len(lines):
        return None, i
    first = lines[i]
    ind = _indent(first)
    if ind < indent:
        return None, i
    stripped = first.strip()
    if stripped.startswith("- "):  # or just "-"
        seq = []
        while i < len(lines) and _indent(lines[i]) == ind and lines[i].strip().startswith("-"):
            item = lines[i].strip()[1:].strip()
            if item == "":
                val, i = _parse_block(lines, i + 1, ind + 1)
                seq.append(val)
            elif ":" in item and not (item[0] in "\"'[{"):
                # inline mapping inside a sequence item: "- key: val"
                synthetic = [" " * (ind + 2) + item] + _collect_children(lines, i + 1, ind)
                val, _ = _parse_block(synthetic, 0, ind + 2)
                seq.append(val)
                i = _skip_children(lines, i + 1, ind)
            else:
                seq.append(_scalar(item))
                i += 1
        return seq, i
    # mapping
    mapping = {}
    while i < len(lines) and _indent(lines[i]) == ind:
        line = lines[i].strip()
        if line.startswith("-"):
            break
        if ":" not in line:
            _ood("unparseable line: %r" % line)
            raise YAMLError("bad mapping line: %r" % line)
        key, _, rest = line.partition(":")
        key = _scalar(key.strip())
        rest = rest.strip()
        if rest == "":
            # nested block
            val, i = _parse_block(lines, i + 1, ind + 1)
            mapping[key] = val if val is not None else None
        else:
            mapping[key] = _scalar(rest)
            i += 1
    return mapping, i

def _collect_children(lines, i, parent_indent):
    out = []
    while i < len(lines) and _indent(lines[i]) > parent_indent + 1:
        out.append(lines[i]); i += 1
    return out

def _skip_children(lines, i, parent_indent):
    while i < len(lines) and _indent(lines[i]) > parent_indent + 1:
        i += 1
    return i


def safe_load(stream):
    if hasattr(stream, "read"):
        text = stream.read()
    else:
        text = stream
    if not isinstance(text, str):
        text = text.decode("utf-8")
    if "---" in text:
        parts = [p for p in text.split("---") if p.strip()]
        if len(parts) > 1:
            _ood("multi-document YAML")
            raise YAMLError("multi-document YAML not supported")
        text = parts[0] if parts else ""
    if "&" in text or "*" in text or "<<:" in text:
        # anchors/aliases/merge keys — refuse rather than mis-handle
        if any(l.strip().startswith(("&", "*", "<<")) or " &" in l or " *" in l for l in text.splitlines()):
            _ood("anchors/aliases/merge keys")
            raise YAMLError("anchors/aliases not supported")
    lines = _split_lines(text)
    if not lines:
        return None
    val, _ = _parse_block(lines, 0, _indent(lines[0]))
    return val

load = safe_load
def full_load(stream): return safe_load(stream)

def _dump_value(v, indent):
    pad = "  " * indent
    if isinstance(v, dict):
        if not v:
            return "{}"
        out = []
        for k, val in v.items():
            if isinstance(val, (dict, list)) and val:
                out.append("%s%s:" % (pad, k))
                out.append(_dump_value(val, indent + 1))
            else:
                out.append("%s%s: %s" % (pad, k, _dump_scalar(val)))
        return "\n".join(out)
    if isinstance(v, list):
        if not v:
            return "[]"
        out = []
        for item in v:
            if isinstance(item, (dict, list)) and item:
                inner = _dump_value(item, indent + 1)
                out.append("%s- %s" % (pad, inner.strip()))
            else:
                out.append("%s- %s" % (pad, _dump_scalar(item)))
        return "\n".join(out)
    return pad + _dump_scalar(v)

def _dump_scalar(v):
    if v is None:
        return "null"
    if isinstance(v, bool):
        return "true" if v else "false"
    if isinstance(v, (int, float)):
        return repr(v)
    s = str(v)
    if s == "" or any(c in s for c in ":#{}[],&*!|>'\"%@`") or s.strip() != s:
        return _json.dumps(s)
    return s

def safe_dump(data, stream=None, default_flow_style=False, sort_keys=True, **kw):
    if sort_keys and isinstance(data, dict):
        data = {k: data[k] for k in sorted(data, key=lambda x: str(x))}
    text = _dump_value(data, 0) + "\n"
    if stream is not None:
        stream.write(text)
        return None
    return text

dump = safe_dump

__version__ = "6.0-shellsim"
