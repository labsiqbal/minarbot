# Test kit fixtures

- `clerk-test-key.pem` / `clerk-test-key.pub.pem`: RSA key pair generated for
  tests only (`openssl genpkey -algorithm RSA -pkeyopt rsa_keygen_bits:2048`).
  It signs fake Clerk session tokens for canonical-mode servers and protects
  nothing. Never use it outside tests.
- `canonical-pre-collapse.sql`: canonical-mode schema fixture, copied from
  `app/test-support/fixtures/canonical-pre-collapse.sql` for #89.
