#!/usr/bin/env python3
import argparse
import logging
import os
import shlex
import subprocess
import sys
from typing import Dict
from typing import List
from typing import NamedTuple

logging.basicConfig(level=os.getenv("LOG_LEVEL", "WARNING"))


def _get_labels(cls, default):
    if default is not None or cls is None:
        return default

    if "typing" in sys.modules and issubclass(cls, NamedTuple):
        return list(cls._field_types.keys())

    # collections.namedtuple
    if hasattr(cls, "_fields"):
        return list(cls._fields)

    return None


def _lines_to_records(lines, fs=None, labels=None, cls=None):
    if fs is None:
        if cls is None:
            return lines
        return [cls(line) for line in lines]
    rows: List[List[str]] = [line.split(fs) for line in lines]

    labels = _get_labels(cls, labels)
    if labels is None:
        if cls is None:
            return rows
        return [cls(*fields) for fields in rows]

    records: List[Dict[str, str]] = [dict(zip(labels, fields)) for fields in rows]
    if cls is None:
        return list(records)

    return [cls(**record) for record in records]


def _git(cmd, *args, fs=None, labels=None, cls=None, check=True):
    quoted_args = " ".join(shlex.quote(arg) for arg in args)
    logging.info(f"> git {cmd} {quoted_args}")
    result = subprocess.run(
        ["git", cmd, *args], stdout=subprocess.PIPE, stderr=subprocess.PIPE,
    )

    if len(result.stderr) != 0:
        try:
            logging.warning(result.stderr.decode())
        except Exception:
            logging.warning(result.stderr)
    try:
        stdout = result.stdout.decode().strip()
        logging.debug(stdout)
    except Exception as e:
        logging.error("Can't decode stdout")
        logging.error(result.stdout)
        raise e

    if check:
        result.check_returncode()
    else:
        return None

    lines = stdout.splitlines()

    return _lines_to_records(lines, fs, labels, cls)


def _branch(*args, format=None, **kwargs):
    if format:
        return _git("branch", "--format", format, *args, **kwargs)
    else:
        return _git("branch", *args, **kwargs)


def _config(*args, default=""):
    result = _git("config", "--default", default, ".".join(args))
    if len(result) == 1:
        return result[0]
    else:
        return None


def _get_push(branch):
    pushremote = _config("branch", branch, "pushremote")
    if pushremote:
        return pushremote

    pushdefault = _config("remote", "pushdefault")
    if pushdefault:
        return pushdefault

    remote = _config("branch", branch, "remote")
    return remote


class LocalBranch(NamedTuple):
    # refs/HEAD/master
    #           ^----^ refname
    refname: str = "refname:short"

    # refs/remotes/origin/feature/tests
    # ^-------------------------------^ {upstream,push}_ref
    #              ^------------------^ {upstream,push}_shortref
    #              ^----^ {upstream,push}
    #                     ^-----------^ {upstream,push}_remoteref
    upstream: str = "upstream:remotename"
    upstream_ref: str = "upstream"
    upstream_shortref: str = "upstream:short"
    upstream_remoteref: str = "upstream:lstrip=3"
    upstream_track: str = "upstream:track"
    push: str = "push:remotename"
    push_ref: str = "push"
    push_shortref: str = "push:short"
    push_remoteref: str = "push:lstrip=3"
    push_track: str = "push:track"


class RemoteBranch(NamedTuple):
    # refs/HEAD/master
    #           ^----^ refname
    refname: str = "refname:short"
    refname_ambiguous: str = "refname:lstrip=3"


def _get_local_branches() -> List[LocalBranch]:
    return _branch(
        format=":".join(f"%({atom})" for atom in LocalBranch._field_defaults.values()),
        fs=":",
        cls=LocalBranch,
    )


def _get_remote_branches() -> List[RemoteBranch]:
    return _branch(
        format=":".join(f"%({atom})" for atom in RemoteBranch._field_defaults.values()),
        fs=":",
        cls=RemoteBranch,
    )


def _branches_to_remove(base, local_branches):
    local_merged = set()
    local_gone = set()
    remotes_merged = dict()
    remotes_gone = dict()
    for branch in local_branches:
        if branch.upstream_shortref == base:
            continue

        merged = len(_git("cherry", base, branch.refname)) == 0
        if merged:
            local_merged.add(branch.refname)
        elif branch.push_track == "[gone]":
            # push is gone, but not merged
            local_gone.add(branch.refname)

        if branch.push_track != "[gone]":
            if merged:
                remotes_merged.setdefault(branch.push, set()).add(
                    branch.push_remoteref,
                )
            elif branch.upstream_track == "[gone]":
                # upstream is gone but not merged
                remotes_gone.setdefault(branch.push, set()).add(branch.push_remoteref)

    return {
        "local": {"merged": local_merged, "gone": local_gone},
        "remotes": {
            "merged": remotes_merged,
            # TODO: never used
            "gone": remotes_gone,
        },
    }


def get_base(local_branches):
    base_name = _config("cleanup", "base", default="master")
    local_base = None
    for branch in local_branches:
        if branch.refname == base_name:
            local_base = branch.upstream_shortref
    if local_base:
        return local_base

    remote_branches = _get_remote_branches()
    for branch in remote_branches:
        if branch.refname == base_name:
            return base_name

    candidates = [br for br in remote_branches
                 if br.refname_ambiguous == base_name]
    if len(candidates) == 0:
        print(f"There is no remote reference matching with: {base_name}", file=sys.stderr, flush=True)
        exit(-1)
    elif len(candidates) >= 2:
        print(f"There are ambiguous remotes with ref: {base_name}", file=sys.stderr, flush=True)
        for candidate in candidates:
            print(f" * {candidate.refname}", file=sys.stderr, flush=True)
        exit(-1)
    return candidates[0]


def get_branches_to_remove(base = None):
    local_branches = _get_local_branches()
    base = base or get_base(local_branches)
    return _branches_to_remove(base, local_branches)


def main():
    parser = argparse.ArgumentParser("cleanup gone tracking branches")
    parser.add_argument("--update", dest="update", action="store_true")
    parser.add_argument("--no-update", dest="update", action="store_false")
    parser.add_argument("--base", type=str)
    parser.set_defaults(update=True)

    args = parser.parse_args()

    if args.update:
        _git("remote", "update", "--prune")

    print("Gone tracking branches:")
    to_remove = get_branches_to_remove(args.base)
    print(to_remove)

    # TODO: remove


if __name__ == "__main__":
    main()
