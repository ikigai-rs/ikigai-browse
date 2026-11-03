"""Browser regression for the real tree face. See ACCESSIBILITY.md for setup."""
import argparse
import json
from pathlib import Path

from playwright.sync_api import sync_playwright

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument("preview", type=Path)
parser.add_argument("axe", type=Path, help="local axe-core JavaScript bundle")
parser.add_argument("--chromium", help="optional Chromium executable")
parser.add_argument("--host-css", type=Path, help="optional complete host stylesheet")
args = parser.parse_args()

# Relevant gonk host defaults, before browse's layout. This intentionally gives
# buttons a large target: browse's more specific reset must not shrink explain
# controls below 24px. The second case checks a host with no button styling.
gonk_defaults = """
:root {font-family:system-ui,sans-serif;line-height:1.5}
*, *::before, *::after {box-sizing:border-box}
button {font:inherit;cursor:pointer;border-radius:.375rem;padding:.45rem .9rem;
        border:1px solid blue;background:blue;color:white;min-height:2.5rem}
"""
host_css = args.host_css.read_text() if args.host_css else gonk_defaults
failures = []
with sync_playwright() as p:
    browser = p.chromium.launch(executable_path=args.chromium)
    page = browser.new_page()
    page.set_content(args.preview.read_text())
    tree = page.locator(".preview-frame").first.inner_html()
    layout = page.locator("style").nth(1).inner_text()
    assert 'browse-entries' in tree and 'browse-explain-link' in tree
    print(json.dumps({"browser": browser.version}))
    for host, css in [("gonk", host_css), ("unstyled", "")]:
        for scheme in ["light", "dark"]:
            for font_size in [12, 16, 20]:
                page.emulate_media(color_scheme=scheme)
                page.set_viewport_size({"width": 320, "height": 900})
                page.set_content('<!doctype html><html lang="en"><title>Tree target test</title>'
                                 '<main id="browse"></main></html>')
                page.locator("#browse").evaluate("(el, tree) => el.innerHTML = tree", tree)
                if css:
                    page.add_style_tag(content=css)
                page.add_style_tag(content=f":root {{font-size:{font_size}px}}")
                page.add_style_tag(content=layout)
                page.add_script_tag(path=str(args.axe.resolve()))
                result = page.evaluate("""async () => {
                    const audit = await axe.run('#browse', {
                        runOnly: {type:'rule', values:['target-size']}
                    });
                    const buttons = [...document.querySelectorAll('.browse-explain-link')];
                    return {axe:axe.version, count:buttons.length,
                        undersized:buttons.filter(el => {
                            const r = el.getBoundingClientRect();
                            return r.width < 24 || r.height < 24;
                        }).map(el => el.outerHTML),
                        violations:audit.violations, incomplete:audit.incomplete};
                }""")
                assert result["count"] > 1, "must test header and row explain controls"
                summary = {"host":host, "scheme":scheme, "font_size":font_size,
                           "axe":result["axe"], "controls":result["count"],
                           "undersized":len(result["undersized"]),
                           "target_size_violations":sum(len(v["nodes"]) for v in result["violations"]),
                           "incomplete":len(result["incomplete"])}
                print(json.dumps(summary))
                if result["undersized"] or result["violations"] or result["incomplete"]:
                    failures.append({**summary, "details":result})
    browser.close()
assert not failures, json.dumps(failures, indent=2)
