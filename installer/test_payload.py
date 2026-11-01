"""Exercise the compiled bootstrapper without opening UI or starting NSIS."""
import argparse
import hashlib
import json
import pathlib
import subprocess
import tempfile


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('--installer', required=True, type=pathlib.Path)
    parser.add_argument('--payload', required=True, type=pathlib.Path)
    args = parser.parse_args()
    payload = args.payload.read_bytes()
    before = set(pathlib.Path(tempfile.gettempdir()).glob('Loquara-setup-*'))
    result = subprocess.run([str(args.installer), '--verify-payload'], capture_output=True, text=True, timeout=30)
    assert result.returncode == 0, result.stderr
    evidence = json.loads(result.stdout)
    assert evidence['verified'] and evidence['extracted']
    assert evidence['bytes'] == len(payload)
    assert evidence['sha256'] == hashlib.sha256(payload).hexdigest()
    assert evidence['installationStarted'] is False
    assert set(pathlib.Path(tempfile.gettempdir()).glob('Loquara-setup-*')) == before, 'Extraction leaked its temporary directory'
    invalid = subprocess.run([str(args.installer), '--install', 'untrusted.exe'], capture_output=True, timeout=10)
    assert invalid.returncode == 2, 'Unknown arguments must exit without UI or execution'
    binary = bytearray(args.installer.read_bytes())
    offset = binary.find(payload)
    assert offset >= 0, 'Embedded bytes differ from the supplied payload'
    binary[offset + len(payload) // 2] ^= 1
    directory = pathlib.Path(tempfile.mkdtemp(prefix='Loquara-payload-test-'))
    tampered = directory / 'tampered.exe'
    try:
        tampered.write_bytes(binary)
        rejected = subprocess.run([str(tampered), '--verify-payload'], capture_output=True, text=True, timeout=30)
        assert rejected.returncode == 1
        assert json.loads(rejected.stdout) == {'verified': False, 'installationStarted': False}
        assert set(pathlib.Path(tempfile.gettempdir()).glob('Loquara-setup-*')) == before, 'Failed verification leaked extraction'
    finally:
        tampered.unlink(missing_ok=True)
        directory.rmdir()
    print(json.dumps({'passed': ['embedded length and SHA256', 'verified disk extraction', 'temporary cleanup',
                                 'unknown arguments rejected', 'tampered payload rejected and cleaned'],
                      'evidence': evidence}, indent=2))


if __name__ == '__main__':
    main()
