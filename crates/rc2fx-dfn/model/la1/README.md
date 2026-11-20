# DeepFilterNet3 LA1 weights

This is the lookahead-1 export, compiled alongside the default LA0 model in the parent directory. `DeepFilter::for_lookahead(1)` loads it when a path's `Suppression.lookahead` setting is set to 1 (default is 0, LA0). It uses the same `int8` kernels as LA0 and adds one extra hop of delay (240 samples, 5.0 ms).

See [the parent README](../README.md) for the binary format, framing tensors, weight replacement steps, and reference regeneration. This export uses a 960 / 360 / 240 / 1 framing (FFT, synthesis, hop, lookahead), and `config.ini` holds the exact training config.

Like LA0, it is DeepFilterNet3 with 128-unit GRUs (`emb_hidden_dim = df_hidden_dim = 128`, `nb_df = 64`, 601,660 parameters), retrained on the same datasets credited in [PROVENANCE.md](../PROVENANCE.md#training-data) with clicks and keystrokes weighted to a quarter of the noise. These weights are the best validation-loss checkpoint from run `a960s360d64-la1-b20` (epoch 102, loss 0.9706).

`reference.wav` comes from running the test signal in `src/tests.rs` through DeepFilterNet's own runtime (libDF with tract, all stages forced on) using these weights exported to ONNX. `preserved_la1_matches_its_own_reference` checks our engine against it; with `int8` weights it lands around −45 dB against the reference, well within the 128 LSB test limit.

- `weights.bin`: `55dec3128a987340686b0110d928a2f655c7425d804e99fcca3a4e0c4b8b36db`
- `reference.wav`: `990af56117eb24f37df77909ddf55649026943b7863fd1dd3feb51cadf98d3f4`
