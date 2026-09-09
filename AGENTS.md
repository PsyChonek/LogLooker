# LogLooker project instructions

## CI/CD

- Use GitHub-hosted Windows runners (`windows-latest`) for this project. Do not
  register or target self-hosted runners, including while the repository is private.
- Follow SqlPlanForDummies' manual release flow: Actions -> Release -> Run workflow
  on `main`, with a patch/minor/major bump. This project intentionally uses
  `workflow_dispatch` for releases instead of a local tag-triggered release command.
- CI runs typecheck, lint, frontend/script tests, frontend build and Rust tests.
- Releases build x64 and ARM64 Windows installers and update both package
  manifests and lockfiles. Winget submission requires a public repository and
  `WINGET_TOKEN` in the `release` environment.
- Follow SqlPlanForDummies' winget flow: sync the fork, run `komac update`
  without submission, remove `InstallerLocale`, run `winget validate`, then
  submit the validated manifests with `komac submit`.
- Pushes, release dispatches, history rewrites and repository visibility changes
  require an explicit user request. Setting up workflows does not authorize a release.
