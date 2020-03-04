# git-trim(1) -- Trim git tracking branches

## SYNOPSIS

`git-trim`

`git-trim --delete all --dry-run`

## DESCRIPTION

`git-trim` trims your tracking branches whose upstream branches are merged or gone.

## FLAGS

--dry-run

Do not delete branches, show what branches will be deleted.

-h, --help

Prints help information.

--no-confirm

Do not ask confirm. [config: trim.confirm]

--no-detach

Do not detach when HEAD is about to be deleted. [config: trim.detach]

--no-update

Not update remotes. [config: trim.update]

-V, --version

Prints version information

## OPTIONS

-b, --bases &lt;bases&gt;...

Comma separated or multiple arguments of refs that other refs are compared to determine whether it is merged or gone.
[default: master][config: trim.base]

-d, --delete &lt;delete&gt;

Comma separated values of '&lt;filter unit&gt;[:&lt;remote name&gt;]'. Filter unit is one of the 'all, merged, gone,
local, remote, merged-local, merged-remote, gone-local, gone-remote'.

- 'all' implies 'merged-local,merged-remote,gone-local,gone-remote'.
- 'merged' implies 'merged-local,merged-remote'.
- 'gone' implies 'gone-local,gone-remote'.
- 'local' implies 'merged-local,gone-local'.
- 'remote' implies 'merged-remote,gone-remote'.

You can scope a filter unit to specific remote ':&lt;remote name&gt;' to a 'filter unit' if the filter unit
implies 'merged-remote' or 'gone-remote'. If there are filter units that are scoped, it trims remote branches only in the specified remote.
If there are any filter unit that isn't scoped, it trims all remote branches. [default : 'merged'] [config: trim.filter]

-p, --protected &lt;protected&gt;...

Comma separated or multiple arguments of glob pattern of branches that never be deleted
