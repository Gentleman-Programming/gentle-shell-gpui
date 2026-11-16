"""Runs the official Vosk Python API on one recorded WAV file, batch.

Written by the app into its private per-job temp folder and run by the
user's isolated venv Python as `python -I -B vosk_transcribe.py`; the
folder dies with the job. Collects EVERY recognized chunk plus the final
result (not just the last), writes the transcript as one text file, and
reports failures without echoing paths or audio.

Usage: vosk_transcribe.py --model DIR --input AUDIO.wav --output OUT.txt
"""

import argparse
import json
import sys

MIN_RATE = 1


def fail(message):
    # One JSON line on stdout keeps the failure structured and bounded;
    # the app never shows raw tracebacks or paths.
    print(json.dumps({"error": message}))
    raise SystemExit(1)


def main():
    parser = argparse.ArgumentParser(add_help=False)
    parser.add_argument("--model", required=True)
    parser.add_argument("--input", required=True)
    parser.add_argument("--output", required=True)
    args = parser.parse_args()

    try:
        from vosk import Model, KaldiRecognizer, SetLogLevel
    except ImportError:
        fail("vosk is not installed in this Python environment")

    import wave

    try:
        wave_file = wave.open(args.input, "rb")
    except (OSError, EOFError, wave.Error):
        # wave.Error is what a file that is not RIFF raises; it used to escape
        # as a traceback instead of the one bounded JSON failure line.
        fail("the recording could not be read")

    with wave_file:
        rate = wave_file.getframerate()
        channels = wave_file.getnchannels()
        if rate != 16000 or channels != 1:
            fail("the recording is not 16 kHz mono")
        try:
            SetLogLevel(-1)
            model = Model(args.model)
        except Exception:
            fail("the Vosk model could not be loaded")
        recognizer = KaldiRecognizer(model, rate)

        collected = []
        while True:
            chunk = wave_file.readframes(4096)
            if len(chunk) == 0:
                break
            if recognizer.AcceptWaveform(chunk):
                try:
                    partial = json.loads(recognizer.Result())
                except json.JSONDecodeError:
                    continue
                text = partial.get("text", "")
                if text:
                    collected.append(text)
        try:
            final = json.loads(recognizer.FinalResult())
        except json.JSONDecodeError:
            fail("the recognizer failed")
        text = final.get("text", "")
        if text:
            collected.append(text)

    transcript = " ".join(collected).strip()
    if not transcript:
        fail("no speech was recognized")
    with open(args.output, "w", encoding="utf-8") as out:
        out.write(transcript + "\n")


if __name__ == "__main__":
    main()
