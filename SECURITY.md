# Security

If you find a security problem in FilePilot, please do not open a public issue. Report it privately through [GitHub's private vulnerability reporting](https://github.com/movidiu64/FilePilot/security/advisories/new) instead, with the steps to reproduce it and the version affected.

## Release signing

The installers attached to releases come with a `.minisig` signature made by the release build ([minisign](https://jedisct1.github.io/minisign/) format, Ed25519). The built-in updater installs only files that carry it and match the SHA-256 GitHub lists for them. You can check an installer yourself with the public key:

```text
minisign -Vm FilePilot_x.y.z_x64-setup.exe -P RWRewtTei3LE9CeNOYDo5sGnPSA8e8JFCDk2Qxa2LvqhLh4/++YyZ13Y
```

## Answers

You will get an answer as soon as possible; fixes for security problems are released as patch versions and mentioned in the changelog.
