# Validate local dictation in Gentle Shell Desktop

Use this checklist to validate the GPUI mic button with an existing compatible Gentle Shell voice installation. The goal is local speech in the originating draft, never an automatic send. The Desktop app does not install the voice model or a second speech engine.

## Start with an isolated instance

1. Use a build that contains the complete voice integration, not just an adapter-only review branch.
2. In Gentle Shell's terminal, run `/gentle:voice` and choose **Show status and notices**. Check that the model and runtime are both installed. Use the same Gentle Shell installation in Desktop. See [Local dictation](integrations.md#local-dictation) for compatible versions and installation layouts.
3. Start a separate Desktop instance using the [isolated testing guide](development.md#isolated-testing). Do not drive an app someone is using or expose real prompts in screenshots.
4. In tab A, type `Draft A:`. Click the microphone, wait for the recording indicator, say one short phrase, then stop. The result should append to `Draft A:`. The chat stays empty and no request is sent.

## Use real hardware on each platform

| Platform | Check in addition to the basic run |
| --- | --- |
| macOS arm64 first, x64 if available | Use the packaged app. Start with the microphone permission denied; the app should fail visibly without recording. Grant permission and retry. Report which process the OS identified in the permission prompt, if any. |
| Linux x64 natively, arm64 if available | Use a desktop audio session with a default input. Repeat with no usable input device: expect an actionable error and no stuck recording state. Do not use WSL to stand in for native Linux. |
| Windows x64 | Retest the generated bundle with active release `5.0.0-1.6.0` and `gentle-pi` 5.0.0 in its release-local pnpm store, not the earlier source snapshot. In the same tab, confirm a chat roundtrip before testing dictation. Check the normal stop path and cancellation while the recognizer is working. |

## Check where a late result goes

- [ ] Start in tab A, then switch to tab B before the result. B keeps its draft and focus; the result is never inserted into B.
- [ ] Cancel while recording, then repeat during transcription. No transcript is inserted, and the mic is released.
- [ ] Close or replace the originating conversation before transcription finishes. No transcript appears in the replacement, and no voice processes are left running.
- [ ] Use an install without voice assets or an unsupported package version. Desktop does not start recording or download anything.

Report the **OS, architecture, Desktop build, Gentle Shell version, install type**, and pass/fail for each check. Include fixed error messages if helpful, but not audio, dictated text, credentials, or private paths. Use `not tested` for anything you could not exercise.
