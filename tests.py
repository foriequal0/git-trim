import contextlib
import os
import subprocess
import tempfile
import textwrap
from pathlib import Path
from unittest import TestCase
import git_cleanup as cleanup


@contextlib.contextmanager
def fixture_context(fixture_setup):
    pwd = os.getcwd()
    try:
        with tempfile.TemporaryDirectory() as tmpdir:
            os.chdir(tmpdir)
            input = fixture_setup.encode()
            subprocess.run(["/usr/bin/env", "bash", "-xe", "-"], input=input)
            os.chdir(Path(tmpdir) / "local")
            yield
    finally:
        os.chdir(pwd)


def with_fixture(fixture_setup):
    # Set an triangular workflow
    prelude = textwrap.dedent("""
    within() {
        pushd $1 > /dev/null;
        source <(cat)
        popd > /dev/null;
    }
    upstream() { within upstream; }
    origin() { within origin; }
    local() { within local; }

    git init upstream;
    upstream <<EOF
        echo "Hello World!" > README.md;
        git add README.md;
        git commit -m "Initial commit"
    EOF
    git clone upstream origin
    git clone origin local
    local <<EOF
        git config remote.pushdefault origin
        git config push.default current
        git remote add upstream ../upstream
    EOF
    """)

    final_fixture = prelude + textwrap.dedent(fixture_setup)

    def decorator(func):
        def wrapper(*args):
            with fixture_context(final_fixture):
                func(*args)

        return wrapper

    return decorator


class TestStringMethods(TestCase):
    @with_fixture("""
    """)
    def test_upper(self):
        pass
