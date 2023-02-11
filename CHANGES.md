# History

## Version 0.2.0

- Restructure ibundle format to V2 for better efficiency.  This bundle file
  format is incompatible with git-ibundle versions prior to v0.2.0 (implying
  v0.2.0 must be used on both source and destination networks), but repository
  metadata (stored in `ibundle/` directory) remains compatible.

- Remove `git ibundle to-bundle` command.  Git bundle files are too limited to
  make their generation worthwhile.

- Generate a reduced set of prerequisite commits by creating a temporary bundle
  file.  Work around Git's refusal to create an empty bundle.

- Add switch `git-ibundle create --basis-current` for bootstrapping a
  pre-existing pair of mirrored repositories.

## Version 0.1.1

- Force fetching via `git fetch --force` to allow non-fast-forward fetches (such
  as from reworked pull requests).

## Version 0.1.0

- Initial version.
