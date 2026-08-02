# Releasing

## Normal release

1. Merge the validated `staging` branch into `main` through the release pull request.
2. Let the Code Foundry release workflow create the versioned GitHub release.
3. When the release is published, `Release Artifacts` builds the native archives and the container image automatically.
4. Check the GitHub release page for the three archives, their SHA-256 files, and the published container tag.

Each archive contains the `model-gateway` binary, the four example configuration files, `README.md`, `LICENSE`, and `NOTICE`.

The container image is published as:

```text
ghcr.io/0xplayerone/model-gateway:v<version>
```

The image is built for `linux/amd64` and `linux/arm64`.

## Rerun artifact delivery

Use this only when a GitHub release already exists but its artifacts are missing or incomplete:

1. Open **Actions → Release Artifacts → Run workflow**.
2. Enter the exact published tag, such as `v0.15.0`.
3. Start the workflow and wait for the native and container jobs to finish.
4. Confirm the release assets and image tag before distributing the release.

The workflow validates that the tag is an exact semantic version and that the GitHub release exists before building anything. Re-running it replaces same-named assets and leaves unrelated release assets untouched.
