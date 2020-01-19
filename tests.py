import contextlib
import os
import subprocess
import tempfile
import textwrap
from unittest import TestCase


@contextlib.contextmanager
def fixture_context(fixture_setup):
    pwd = os.getcwd()
    try:
        with tempfile.TemporaryDirectory() as tmpdir, \
                tempfile.NamedTemporaryFile() as fixture_setup_tmpfile:
            os.chdir(tmpdir)
            fixture_setup_tmpfile.write(fixture_setup.encode())
            fixture_setup_tmpfile.flush()
            subprocess.run(["/usr/bin/env", "bash", "-ve", fixture_setup_tmpfile.name]).check_returncode()
            yield
    finally:
        os.chdir(pwd)

def with_fixture(fixture_setup):
    def decorator(func):
        def wrapper(*args):
            with fixture_context(textwrap.dedent(fixture_setup)):
                func(*args)
        return wrapper
    return decorator


class TestStringMethods(TestCase):
    @with_fixture("""
    git init origin
    """)
    def test_upper(self):
         pass