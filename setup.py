from setuptools import setup

setup(
    name="git cleanup",
    version="0.1",
    python_requires='>=3.6',
    py_modules=[
        "git_cleanup",
    ],
    entry_points={
        "console_scripts": [
            "git-cleanup = git_cleanup:main"
        ]
    }
)
