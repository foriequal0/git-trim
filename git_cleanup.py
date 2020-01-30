#!/usr/bin/env python3
import argparse
import logging
import os
import re
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


def _get_configs():
    lines = _git("config", "--list")
    configs = {}
    for line in lines:
        try:
            sep = line.index("=")
            key = line[:sep].strip()
            value = line[sep + 1:].strip()
            configs[key] = value
        except ValueError:
            configs[line.strip()] = True
    return configs


def _get_config(configs, *args, default=None, type=str):
    key = ".".join(args)
    result = configs.get(key, default)
    if result is None:
        return None
    if issubclass(type, bool):
        if result is bool:
            return result
        truelikes = {"yes", "on", "true", "1"}
        falselikes = {"no", "off", "false", "0"}
        lower = result.lower()
        if lower in truelikes:
            return True
        elif lower in falselikes:
            return False
        else:
            raise ValueError(f"Invalid boolean: {result}")
    if issubclass(type, int):
        if result is int:
            return result
        match = re.fullmatch(r"^(?P<number>\d+)(?P<suffix>(|k|M|G))$", result)
        if not match:
            raise ValueError(f"Invalid integer: {result}")
        multipliers = {
            "": 1,
            "k": 1024,
            "M": 1024 ** 2,
            "G": 1024 ** 3,
        }
        return int(match.group("number") * multipliers[match.group("suffix")])
    return result


def _get_push(configs, branch):
    # TODO: https://git-scm.com/docs/git-config#Documentation/git-config.txt-pushdefault
    pushremote = _get_config(configs, f"branch.{branch}.pushRemote")
    if pushremote:
        return pushremote

    pushdefault = _get_config(configs, "remote.pushDefault")
    if pushdefault:
        return pushdefault

    remote = _get_config(configs, f"branch.{branch}.remote")
    return remote


class LocalBranch(NamedTuple):
    # refs/HEAD/master
    #           ^----^ refname
    refname: str = "refname:short"

    # TODO: https://git-scm.com/docs/git-pull#_remotes_a_id_remotes_a

    # refs/remotes/origin/feature/tests
    # ^-------------------------------^ {upstream,push}_ref
    #              ^------------------^ {upstream,push}_shortref
    #              ^----^ {upstream,push}
    #                     ^-----------^ {upstream,push}_remoteref
    upstream: str = "upstream:remotename"
    upstream_ref: str = "upstream"
    upstream_shortref: str = "upstream:short"
    ## TODO: parse config remote.<origin>.fetch, $GIT_DIR/remotes/<origin>
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
        # TODO: branch.<name>.merge ?
        # TODO: tag ?
        # TODO: remote tracking? https://git-scm.com/docs/git-fetch#_configured_remote_tracking_branches_a_id_crtb_a
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


def _get_base(configs, local_branches, remote_branches):
    base_name = _get_config(configs, "cleanup.base", default="master")

    local_base = None
    for branch in local_branches:
        if branch.refname == base_name:
            local_base = branch

    # Use a local branch's tracking upstream
    if local_base:
        if local_base.upstream_track == "[gone]":
            shortref = local_base.upstream_shortref
            print(
                f"Tracking upstream branch is gone: {base_name} -> {shortref}",
                file=sys.stderr,
                flush=True,
            )
            exit(-1)
        return local_base.upstream_shortref

    # Use a remote branch
    for branch in remote_branches:
        if branch.refname == base_name:
            return base_name

    # Find a closest remote branch
    # TODO: https://git-scm.com/docs/git-checkout#Documentation/git-checkout.txt-emgitcheckoutemltbranchgt
    candidates = [br for br in remote_branches if br.refname_ambiguous == base_name]
    if len(candidates) == 0:
        print(
            f"There is no remote reference matching with: {base_name}",
            file=sys.stderr,
            flush=True,
        )
        exit(-1)
    elif len(candidates) >= 2:
        print(
            f"There are ambiguous remotes with ref: {base_name}",
            file=sys.stderr,
            flush=True,
        )
        for candidate in candidates:
            print(f" * {candidate.refname}", file=sys.stderr, flush=True)
        exit(-1)
    return candidates[0]


def get_branches_to_remove(base=None):
    configs = _get_configs()
    local_branches = _get_local_branches()
    remote_branches = _get_remote_branches()
    base = base or _get_base(configs, local_branches, remote_branches)
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
