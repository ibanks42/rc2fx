module github.com/ibanks42/rc2fx/tools/xtask

go 1.27

toolchain go1.27.4

require (
	github.com/ibanks42/rc2fx/desktop v0.0.0
	github.com/klauspost/compress v1.20.4
	golang.org/x/sys v0.50.0
	howett.net/plist v1.0.1
)

require (
	github.com/google/go-containerregistry v0.20.7 // indirect
	github.com/opencontainers/go-digest v1.0.0 // indirect
	github.com/secure-systems-lab/go-securesystemslib v0.11.0 // indirect
	github.com/sigstore/protobuf-specs v0.5.0 // indirect
	github.com/sigstore/sigstore v1.10.6 // indirect
	github.com/theupdateframework/go-tuf/v2 v2.4.2 // indirect
	golang.org/x/crypto v0.59.0 // indirect
	golang.org/x/term v0.48.0 // indirect
	google.golang.org/genproto/googleapis/api v0.0.0-20250825161204-c5933d9347a5 // indirect
	google.golang.org/protobuf v1.36.11 // indirect
)

replace github.com/ibanks42/rc2fx/desktop => ../../desktop
