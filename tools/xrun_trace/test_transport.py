import gzip
import os
from types import SimpleNamespace
import unittest
from unittest.mock import patch
import run

class Transport(unittest.TestCase):
    @patch.dict(os.environ,{'RC2FX_MIXER_IP':'test-mixer'})
    def test_download_is_compressed_on_device_and_decoded_on_host(self):
        with patch.object(run.subprocess,'run',return_value=SimpleNamespace(returncode=0,stdout=gzip.compress(b'{"xruns":0}'),stderr=b'')) as call:
            self.assertEqual(run.ssh('cat /run/rc2fx/audit.json'),b'{"xruns":0}')
            command=call.call_args.args[0][-1]
            self.assertIn('gzip -1',command)
            self.assertIn('pipefail',command)
    @patch.dict(os.environ,{'RC2FX_MIXER_IP':'test-mixer'})
    def test_upload_stays_plain(self):
        with patch.object(run.subprocess,'run',return_value=SimpleNamespace(returncode=0,stdout=b'',stderr=b'')) as call:
            self.assertEqual(run.ssh('cat > /run/diagnostic',b'binary'),b'')
            self.assertEqual(call.call_args.args[0][-1],'cat > /run/diagnostic')
            self.assertEqual(call.call_args.kwargs['input'],b'binary')
    @patch.dict(os.environ,{'RC2FX_MIXER_IP':'test-mixer'})
    def test_remote_failure_is_not_treated_as_valid_empty_output(self):
        with patch.object(run.subprocess,'run',return_value=SimpleNamespace(returncode=1,stdout=b'',stderr=b'missing audit')):
            with self.assertRaisesRegex(RuntimeError,'missing audit'):run.ssh('cat missing')
