Test-only RSA key pairs used to sign fake Entra tokens in `tests/entra_jwt.rs`.
They are not used anywhere outside the tests and protect nothing.
`test-rsa-N.pub.json` holds the base64url JWK modulus (`n`) and exponent (`e`).
