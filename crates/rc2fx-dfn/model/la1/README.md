# DeepFilterNet3 LA1 weights

The lookahead-1 export, embedded alongside the default LA0 model in the parent
directory. `DeepFilter::for_lookahead(1)` loads it, and the saved per-path
setting `Suppression.lookahead` selects it (the default is 0, LA0). LA1 adds
exactly one hop of delay (240 samples, 5.0 ms) and uses the same int8 kernels.

The file format, the framing tensors, and the steps for replacing the weights
and regenerating the reference are in [the parent README](../README.md). This
export is 960 / 360 / 240 / 1 (FFT, synthesis, hop, lookahead). `config.ini`
is the exact run configuration.

The model is DeepFilterNet3 with the GRUs narrowed to 128 units
(`emb_hidden_dim = df_hidden_dim = 128`, `nb_df = 64`, 601,660 parameters),
retrained by RC2FX on DNS-4 speech and noise, VCTK, room impulse responses,
and mouse-click and keyboard recordings (Freesound, FSD50K, ESC-50, AudioSet)
weighted to a quarter of the noise. The weights are the best checkpoint by
validation loss from run `a960s360d64-la1-b20` (epoch 102, 0.9706).

`reference.wav` is the output of DeepFilterNet's own runtime (libDF with
tract, every stage forced on) for the test signal in `src/tests.rs`, using
these weights exported to ONNX. `preserved_la1_matches_its_own_reference`
compares the engine with it. With int8 weights the engine measures about
−45 dB against the reference, within the 128 LSB test limit.

- `weights.bin`: `55dec3128a987340686b0110d928a2f655c7425d804e99fcca3a4e0c4b8b36db`
- `reference.wav`: `990af56117eb24f37df77909ddf55649026943b7863fd1dd3feb51cadf98d3f4`
