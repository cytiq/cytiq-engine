import tempfile
from pathlib import Path
import unittest

from tag_validated_release import git, prepare_tag


class ValidatedReleaseTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.root = Path(self.temporary.name)
        git(self.root, "-c", "init.templateDir=", "init", "-b", "main")
        for key, value in {"user.name":"Test", "user.email":"test@example.invalid",
                           "commit.gpgsign":"false", "tag.gpgsign":"false",
                           "core.hooksPath":"/dev/null"}.items():
            git(self.root, "config", key, value)
        self.first = self.commit("one")
        self.second = self.commit("two")

    def tearDown(self):
        self.temporary.cleanup()

    def commit(self, value):
        (self.root/"source").write_text(value)
        git(self.root, "add", "source")
        git(self.root, "commit", "-m", value)
        return git(self.root, "rev-parse", "HEAD").stdout.strip()

    def test_main_advancing_does_not_change_the_validated_release_target(self):
        git(self.root, "checkout", "--detach", self.first)
        prepare_tag(self.root, "0.4.1", self.first, sign=False)
        self.assertEqual(git(self.root,"rev-parse","main").stdout.strip(),self.second)
        self.assertEqual(git(self.root,"rev-parse","0.4.1^{commit}").stdout.strip(),self.first)

    def test_wrong_checkout_cannot_be_published(self):
        with self.assertRaises(ValueError):prepare_tag(self.root,"0.4.1",self.first,sign=False)
        self.assertNotEqual(git(self.root,"rev-parse","--verify","refs/tags/0.4.1",check=False).returncode,0)

    def test_retry_is_idempotent_and_conflicting_tags_are_immutable(self):
        prepare_tag(self.root,"0.4.1",self.second,sign=False)
        before=git(self.root,"rev-parse","refs/tags/0.4.1").stdout
        prepare_tag(self.root,"0.4.1",self.second,sign=False)
        self.assertEqual(git(self.root,"rev-parse","refs/tags/0.4.1").stdout,before)
        git(self.root,"checkout","--detach",self.first)
        with self.assertRaises(ValueError):prepare_tag(self.root,"0.4.1",self.first,sign=False)
        self.assertEqual(git(self.root,"rev-parse","refs/tags/0.4.1").stdout,before)

    def test_arguments_are_validated_before_git_mutation(self):
        for version,commit in [("--delete",self.second),("0.4.1; command",self.second),("0.4.1","--all")]:
            with self.assertRaises(ValueError):prepare_tag(self.root,version,commit,sign=False)
        self.assertEqual(git(self.root,"tag","--list").stdout,"")


if __name__ == "__main__":unittest.main()
