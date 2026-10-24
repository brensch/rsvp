#!/usr/bin/env python3
"""Convert the wedding repo's guests/guest-list.csv into `rsvp household import` JSON.

    python3 tools/import_wedding_csv.py ~/wedding/guests/guest-list.csv > households.json
    rsvp household import households.json

Mapping:
  - rows are grouped by the Household column (or the guest's own name if blank)
  - "X's +1" / "X +1" placeholder rows become `plus_one_allowed` on their host
  - Wedding=Yes-R1 -> wedding; Rehearsal Dinner=Yes -> rehearsal-dinner
  - a person invited to fewer events than their household gets a per-person override
  - Category -> side (Bride's -> a, Groom's -> b, Parent Requests -> a) and a tag;
  - australia: anyone in the household is down for the Melbourne event
  - tier: 1 if anyone in the household is Wedding=Yes-R1, 2 if anyone is TBD, otherwise 3
  - the couple's own rows (Bride, Groom) and the totals row are skipped
"""
import csv
import json
import re
import sys
from collections import OrderedDict

TITLES = {"aunt", "uncle", "grandma", "grandpa", "nana", "pop", "cousin"}


def is_placeholder(name: str) -> bool:
    return "+1" in name


def split_name(name: str):
    parts = name.strip().split()
    if len(parts) <= 1 or parts[0].lower() in TITLES or "'s" in name:
        return name.strip(), ""
    return " ".join(parts[:-1]), parts[-1]


def person_events(row):
    ev = []
    if row["Rehearsal Dinner"].strip() == "Yes":
        ev.append("rehearsal-dinner")
    if row["Wedding"].strip() == "Yes-R1":
        ev.append("wedding")
    return ev


def side_and_tag(category: str):
    c = category.lower()
    if c.startswith("bride"):
        return "a", "family" if "family" in c else "friends"
    if c.startswith("groom"):
        return "b", "family" if "family" in c else "friends"
    if "parent" in c:
        return "a", "parent-requests"
    return "both", ""


def main(path):
    rows = [r for r in csv.DictReader(open(path, newline="")) if r["Category"].strip() not in ("", "Bride", "Groom")]
    groups = OrderedDict()
    for r in rows:
        key = r["Household"].strip() or r["Guest Name"].strip()
        groups.setdefault(key, []).append(r)

    out = []
    for key, members in groups.items():
        real = [m for m in members if not is_placeholder(m["Guest Name"])]
        placeholders = [m for m in members if is_placeholder(m["Guest Name"])]
        if not real:
            continue
        side, tag = side_and_tag(real[0]["Category"])
        tags = [t for t in [tag] if t]
        wedding = {m["Wedding"].strip() for m in real}
        tier = 1 if "Yes-R1" in wedding else 2 if "TBD" in wedding else 3

        household_events = []
        for m in real + placeholders:
            for e in person_events(m):
                if e not in household_events:
                    household_events.append(e)
        order = ["rehearsal-dinner", "wedding"]
        household_events.sort(key=order.index)

        # Whose +1 is each placeholder? Prefer the row naming it in "Partner / +1".
        hosts = set()
        for p in placeholders:
            host = next((m for m in real if m["Partner / +1"].strip() == p["Guest Name"].strip()), None)
            if host is None:
                stem = re.sub(r"('s)?\s*\+1\??$", "", p["Guest Name"]).strip().lower()
                host = next((m for m in real if m["Guest Name"].lower().startswith(stem)), real[0])
            hosts.add(id(host))

        guests = []
        for i, m in enumerate(real):
            first, last = split_name(m["Guest Name"])
            g = {"first_name": first, "last_name": last, "sort": i}
            if id(m) in hosts:
                g["plus_one_allowed"] = True
            own = person_events(m)
            if own != household_events:
                g["events"] = own  # a subset; [] = on the list but not invited to these events
            notes = [m["Notes"].strip()] if m["Notes"].strip() else []
            if m["Email"].strip():
                g["email"] = m["Email"].strip()
            if m["Dietary Restrictions"].strip():
                g["dietary"] = m["Dietary Restrictions"].strip()
            if notes:
                g["notes"] = " · ".join(notes)
            guests.append(g)

        firsts = [g["first_name"] for g in guests]
        greeting = firsts[0] if len(firsts) == 1 else ", ".join(firsts[:-1]) + " & " + firsts[-1]
        h = {
            "name": key,
            "greeting": greeting,
            "side": side,
            "tier": tier,
            "australia": any(m["Melbourne Event"].strip() == "Yes" for m in real),
            "tags": ", ".join(tags),
            "events": household_events,
            "guests": guests,
        }
        addr = real[0]["Address"].strip()
        if addr:
            h["addr1"] = addr
            h["city"] = real[0]["City State ZIP"].strip()
        out.append(h)

    json.dump(out, sys.stdout, indent=2)
    print(f"{len(out)} households, {sum(len(h['guests']) for h in out)} people", file=sys.stderr)


if __name__ == "__main__":
    main(sys.argv[1])
