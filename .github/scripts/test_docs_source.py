"""Offline checks for release/manual docs source selection."""

import unittest

from docs_source import source_ref


class DocsSourceTests(unittest.TestCase):
    def test_manual_run_resolves_main_without_fetching_releases(self):
        def forbidden():
            self.fail("manual main must not depend on a latest release")
        self.assertEqual(source_ref("main", forbidden), "refs/heads/main")

    def test_rerun_resolves_latest_again_instead_of_reusing_old_tag(self):
        release = {"tag_name": "v1.1.0", "draft": False,
                   "prerelease": False, "published_at": "2026-11-04T12:00:00Z"}
        self.assertEqual(source_ref("latest-release", lambda: release), "refs/tags/v1.1.0")
        release["tag_name"] = "v1.2.0"
        self.assertEqual(source_ref("latest-release", lambda: release), "refs/tags/v1.2.0")

    def test_unpublished_non_app_and_invalid_tags_are_refused(self):
        valid = {"tag_name": "v1.1.0", "draft": False,
                 "prerelease": False, "published_at": "2026-11-04T12:00:00Z"}
        for change in [{"draft": True}, {"prerelease": True}, {"published_at": None},
                       {"tag_name": "mixer-v1.1.0"}, {"tag_name": "v01.1.0"},
                       {"tag_name": "v1.1.0-rc1"}, {"tag_name": "v1.1.0\nref=main"}]:
            with self.subTest(change=change), self.assertRaises(ValueError):
                source_ref("latest-release", lambda: valid | change)
        with self.assertRaises(ValueError):
            source_ref("v1.0.0", lambda: valid)


if __name__ == "__main__":
    unittest.main()
