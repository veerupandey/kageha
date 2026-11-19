import base64
import time
import unittest
from kageha_client.credentials import CredentialRef, CredentialAccess, CredentialClient, SecretLease
class CredentialsTest(unittest.TestCase):
    def test_redacted_and_disposed(self):
        lease=SecretLease(b'CANARY',{},int(time.time()*1000)+5000)
        view=lease.expose()
        self.assertNotIn('CANARY',repr(lease))
        lease.close()
        self.assertEqual(bytes(view),b'\0'*6)
        with self.assertRaises(Exception): lease.expose()
    def test_client_scope_and_operation_ids(self):
        calls=[]
        def request(route,body):
            calls.append((route,body))
            if route=='/v1/resolve':
                return {'metadata':{'reference':{'id':'primary','namespace':'models'}},'secret_base64':base64.b64encode(b'CANARY').decode(),'valid_until_ms':int(time.time()*1000)+5000}
            return {}
        client=CredentialClient(request)
        ref=CredentialRef('models','primary');access=CredentialAccess('model','https://provider.example')
        with client.resolve(ref,access) as lease: self.assertEqual(bytes(lease.expose()),b'CANARY')
        client.put(ref,access,operation_id='stable',expected_revision=0,kind='api_key',value=b'CANARY')
        self.assertEqual(calls[-1][1]['operation_id'],'stable')
        self.assertNotIn('principal',calls[-1][1])
        client.replacement(access,{'action':'status','operation_id':'switch'})
        self.assertEqual(calls[-1][0],'/v1/replacements')
if __name__=='__main__': unittest.main()
