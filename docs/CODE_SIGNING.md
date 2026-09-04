# Release signing

The default GitHub Actions workflow builds unsigned Windows installers and ad-hoc signed macOS bundles. It does not require repository secrets and never writes credentials into build artifacts or logs.

## macOS signing and notarization

For trusted distribution outside the Mac App Store, create an Apple Developer ID Application certificate and configure these encrypted GitHub Actions secrets:

- `APPLE_CERTIFICATE`: base64-encoded `.p12` certificate
- `APPLE_CERTIFICATE_PASSWORD`: certificate password
- `APPLE_SIGNING_IDENTITY`: certificate identity, such as `Developer ID Application: ...`
- `APPLE_ID`: Apple developer account email
- `APPLE_PASSWORD`: app-specific password
- `APPLE_TEAM_ID`: Apple Developer Team ID

Do not commit any certificate, private key, password, provisioning profile, or `.env` file. Update the workflow to expose these values only through its `env` mapping after the secrets have been configured.

## Windows signing

Windows releases can be built without signing, but SmartScreen may warn users. For production distribution, use a trusted code-signing certificate or a managed signing service. Store the certificate and password as encrypted GitHub Actions secrets, never as repository files.

See the official [Tauri code signing guide](https://v2.tauri.app/distribute/sign/) before enabling either platform's signing configuration.
