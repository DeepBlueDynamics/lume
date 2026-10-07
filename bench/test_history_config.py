"""Independent TOML parse oracle for the plugin's surgical configuration edit."""
import json
import pathlib
import shutil
import subprocess
import unittest

try:
    import tomllib
except ImportError:
    tomllib = None

ROOT = pathlib.Path(__file__).resolve().parents[1]


@unittest.skipUnless(tomllib is not None and shutil.which("node"), "requires Python 3.11 and Node")
class HistoryConfigTomlTests(unittest.TestCase):
    def test_existing_values_are_preserved_and_only_absent_opt_in_defaults(self):
        fixtures = [
            "",
            "width_seconds=10\n[query]\nmax_rows=300\n",
            'profiles.hr_paths=["navigation.position"]',
            'profiles.hr_paths=[]\n[query]\nmax_rows=300\n',
            '[profiles] # comment\nhr_paths=[]\n',
            '["profiles"]\n"opt_in"=[]\n',
            'profiles = { hr_paths = ["navigation.position"] }\n',
            'profiles = { opt_in = [], hr_paths = [] }\n',
            'profiles={}\n',
            "profiles={'opt_in'=['count']}\n",
            'text="""\n[profiles]\nopt_in=[]\n"""\n',
            'text="""four closing quotes""""\n',
            "text='''five closing quotes'''''\n",
            'text="["\nother="]"\n[profiles]\nhr_paths=[]\n',
            '["query"]\nmax_rows=300\n',
            '"profiles"."hr_paths"=[]\n',
        ]
        script = "const {addLastDefault}=require(process.argv[1]); " + \
                 "let s='';process.stdin.on('data',c=>s+=c);" + \
                 "process.stdin.on('end',()=>console.log(JSON.stringify(JSON.parse(s).map(addLastDefault))));"
        result = subprocess.run(
            ["node", "-e", script, str(ROOT / "plugins/signalk-lume-ti/lib/store-config.js")],
            input=json.dumps(fixtures), text=True, capture_output=True, check=True)
        for before, after in zip(fixtures, json.loads(result.stdout)):
            with self.subTest(config=before):
                expected = tomllib.loads(before)
                expected.setdefault("profiles", {}).setdefault("opt_in", ["last"])
                self.assertEqual(tomllib.loads(after), expected)
