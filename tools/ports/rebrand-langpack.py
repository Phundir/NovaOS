#!/usr/bin/env python3
"""Rewrites the branding of a Firefox language pack for the Web Browser fork.

Mozilla's language packs carry their own brand strings ("Mozilla Firefox")
and, once installed, they win over the fork's in their language: the
window titles and the notices of the French interface said "Firefox".
The brand files are rewritten, and the product name in the rest of the
strings that spell it out instead of using the brand term.

Usage: rebrand-langpack.py <in.xpi> <out.xpi>
"""
import re
import sys
import zipfile

NAME = "Web Browser"

BRAND_FTL = f"""-brand-shorter-name = {NAME}
-brand-short-name = {NAME}
-brand-shortcut-name = {NAME}
-brand-full-name = {NAME}
-brand-product-name = {NAME}
-vendor-short-name = Mozilla
trademarkInfo = {{ " " }}
"""
BRAND_PROPERTIES = f"""brandShorterName={NAME}
brandShortName={NAME}
brandFullName={NAME}
"""


def rewrite(name, data):
    if name.endswith("/branding/brand.ftl"):
        return BRAND_FTL.encode()
    if name.endswith("/branding/brand.properties"):
        return BRAND_PROPERTIES.encode()
    if name.endswith((".ftl", ".properties", ".dtd")):
        text = data.decode("utf-8")
        # Only the product name: "Mozilla Firefox" and a bare "Firefox"
        # in running text. Sub-brands ("Firefox View", "Firefox Relay",
        # "Firefox Monitor"...) name Mozilla services, kept as they are.
        text = text.replace("Mozilla Firefox", NAME)
        text = re.sub(r"\bFirefox\b(?! (?:View|Relay|Monitor|Send|Screenshots|Translations|Focus|Profiler|Labs|Suggest|Account|Accounts|Sync|Private Network))", NAME, text)
        return text.encode("utf-8")
    return data


def main():
    source, target = sys.argv[1], sys.argv[2]
    with zipfile.ZipFile(source) as zin, zipfile.ZipFile(target, "w", zipfile.ZIP_DEFLATED) as zout:
        for info in zin.infolist():
            data = zin.read(info.filename)
            # Identifiers (the add-on id, manifest keys) are left alone.
            if info.filename != "manifest.json":
                data = rewrite(info.filename, data)
            zout.writestr(info, data)


if __name__ == "__main__":
    main()
