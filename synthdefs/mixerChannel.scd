// Compile SynthDefs for termixer
// Run: sclang mixerChannel.scd
(
SynthDef("mixerChannel", { |in=2, out=0, vol=0.8, lpf=20000, hpf=20, eqLowFreq=20000, eqLowGain=0, eqMidFreq=1000, eqMidGain=0, eqHighFreq=20, eqHighGain=0,
    mEq32=0, mEq64=0, mEq125=0, mEq250=0, mEq500=0,
    mEq1k=0, mEq2k=0, mEq4k=0, mEq8k=0, mEq16k=0,
    pan=0|
    var sig = In.ar(in, 2);
    // Smooth OSC-driven params. Raw .kr args jump between control blocks
    // (64 samples default), causing biquad coefficient jumps → clicks/crackle
    // on sweeps. Lag.kr applies a per-control-block exponential smoother.
    // 30 ms is fast enough for responsive knob feel, slow enough to eliminate
    // audible transients when the UI spams param changes.
    var lpfL = Lag.kr(lpf, 0.03);
    var hpfL = Lag.kr(hpf, 0.03);
    var eqLowFreqL = Lag.kr(eqLowFreq, 0.03);
    var eqMidFreqL = Lag.kr(eqMidFreq, 0.03);
    var eqHighFreqL = Lag.kr(eqHighFreq, 0.03);
    var eqLowGainL = Lag.kr(eqLowGain, 0.03);
    var eqMidGainL = Lag.kr(eqMidGain, 0.03);
    var eqHighGainL = Lag.kr(eqHighGain, 0.03);
    var volL = Lag.kr(vol, 0.02);
    var panL = Lag.kr(pan, 0.02);
    // Per-channel EQ: low/high cut with filters, boost with shelves, mid with peak EQ
    sig = LPF.ar(sig, eqLowFreqL);
    sig = BLowShelf.ar(sig, 120, 1.0, eqLowGainL);
    sig = BPeakEQ.ar(sig, eqMidFreqL, 0.7, eqMidGainL);
    sig = HPF.ar(sig, eqHighFreqL);
    sig = BHiShelf.ar(sig, 8000, 1.0, eqHighGainL);
    // 10-band master EQ (small lag on gain args to smooth any bulk updates)
    sig = BPeakEQ.ar(sig, 32, 1.0, Lag.kr(mEq32, 0.03));
    sig = BPeakEQ.ar(sig, 64, 1.0, Lag.kr(mEq64, 0.03));
    sig = BPeakEQ.ar(sig, 125, 1.0, Lag.kr(mEq125, 0.03));
    sig = BPeakEQ.ar(sig, 250, 1.0, Lag.kr(mEq250, 0.03));
    sig = BPeakEQ.ar(sig, 500, 1.0, Lag.kr(mEq500, 0.03));
    sig = BPeakEQ.ar(sig, 1000, 1.0, Lag.kr(mEq1k, 0.03));
    sig = BPeakEQ.ar(sig, 2000, 1.0, Lag.kr(mEq2k, 0.03));
    sig = BPeakEQ.ar(sig, 4000, 1.0, Lag.kr(mEq4k, 0.03));
    sig = BPeakEQ.ar(sig, 8000, 1.0, Lag.kr(mEq8k, 0.03));
    sig = BPeakEQ.ar(sig, 16000, 1.0, Lag.kr(mEq16k, 0.03));
    sig = LPF.ar(sig, lpfL);
    sig = HPF.ar(sig, hpfL);
    sig = sig * volL;
    sig = Balance2.ar(sig[0], sig[1], panL);
    Out.ar(out, sig);
}).writeDefFile("/Users/dwtompkins/Downloads/Tidal/termixer/synthdefs");

SynthDef("monitorChannel", { |in=0, out=2|
    var sig = In.ar(in, 2);
    Out.ar(out, sig);
}).writeDefFile("/Users/dwtompkins/Downloads/Tidal/termixer/synthdefs");

"SynthDefs written!".postln;
0.exit;
)

SynthDef("monitorChannel", { |in=0, out=2|
    var sig = In.ar(in, 2);
    Out.ar(out, sig);
}).writeDefFile("/Users/dwtompkins/Downloads/Tidal/termixer/synthdefs");

"SynthDefs written!".postln;
0.exit;
)
