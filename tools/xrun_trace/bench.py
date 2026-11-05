#!/usr/bin/env python3
"""Two-hour six-channel qualification; device reads only every 15 minutes."""
import argparse
import array
import datetime
import io
import json
import math
from pathlib import Path
import subprocess
import sys
import tarfile
import threading
import time
import run


def utc():
    return datetime.datetime.now(datetime.timezone.utc).isoformat()


def save(path, value):
    temporary = path.with_suffix('.tmp')
    temporary.write_text(json.dumps(value, indent=2)+'\n')
    temporary.replace(path)


def snapshot():
    lines = run.ssh('cat /proc/sys/kernel/random/boot_id; cat /proc/uptime; cat /run/rc2fx/home; cat /sys/kernel/debug/tracing/tracing_on; cat /run/rc2fx/audit.json').decode().splitlines()
    return dict(hostUtc=utc(), bootId=lines[0], uptime=float(lines[1].split()[0]), home=lines[2], tracing=int(lines[3]), audit=json.loads('\n'.join(lines[4:])))


def check(base, previous, current):
    a, b = current['audit'], base['audit']
    errors=[]
    if current['home']!='/opt/rc2fx': errors.append('Runtime is not installed')
    if current['bootId']!=base['bootId'] or a['processPid']!=b['processPid']: errors.append('Mixer restarted')
    if not 0<=current['uptime']-a['monotonicNs']/1e9<5: errors.append('Stale telemetry')
    if a['callbacks']<=previous['audit']['callbacks']: errors.append('Callbacks stopped')
    for key in ['xruns','dspFaults','capacityFailures','planErrors','controlServerErrors']:
        if a[key]!=b[key]: errors.append(key+' changed')
    for key in ['late','stalls']:
        if a['helper'][key]!=b['helper'][key]: errors.append('helper '+key+' changed')
    flight=a['clientTiming']
    if flight['flightToken']!=b['clientTiming']['flightToken']: errors.append('Flight recorder token changed')
    if flight['flightFrozenAtNs'] or not current['tracing']: errors.append('Fault recorder froze')
    return errors


def main():
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--out', type=Path, required=True)
    parser.add_argument('--bundle',type=Path,required=True)
    parser.add_argument('--seconds', type=int, default=7200)
    args=parser.parse_args()
    root=args.out
    root.mkdir(parents=True,exist_ok=False)
    children=[];logs=[];stop=threading.Event()
    state={'state':'STARTING','startedUtc':utc()}
    save(root/'status.json',state)
    def feed(p, samples):
        try:
            while not stop.is_set(): p.stdin.write(samples);p.stdin.flush()
        except (BrokenPipeError,ValueError): pass
    try:
        sinks=json.loads(subprocess.check_output(['pactl','-f','json','list','sinks']))
        sinks=[s for s in sinks if s['name'].startswith('alsa_output.usb-R__DE_RODECaster_Pro_II_GV0044100-00.')]
        if len(sinks)!=6: raise RuntimeError(f'Expected six mixer sinks; found {len(sinks)}')
        records=[]
        for index,(sink,freq) in enumerate(zip(sinks,[196,247,294,330,392,440])):
            samples=array.array('h')
            for i in range(48000*4):
                t=i/48000;envelope=.25+.75*(.5-.5*math.cos(2*math.pi*t/2))
                left=.01*envelope*(.7*math.sin(2*math.pi*freq*t)+.3*math.sin(2*math.pi*freq*2*t))
                right=.01*envelope*(.7*math.sin(2*math.pi*freq*t+.3)+.3*math.sin(2*math.pi*freq*3*t))
                samples.extend((int(left*32767),int(right*32767)))
            log=(root/f'playback-{index}.log').open('w');logs.append(log)
            child=subprocess.Popen(['pw-play','--raw','--format=s16','--rate=48000','--channels=2','--channel-map=FL,FR','--target='+sink['name'],'--properties=media.name="RC2FX diagnostic bench"','-'],stdin=subprocess.PIPE,stdout=log,stderr=log)
            children.append(child)
            threading.Thread(target=feed,args=(child,samples.tobytes()),daemon=True).start()
            records.append(dict(pid=child.pid,sink=sink['name'],description=sink['description'],frequencyHz=freq,peakBoundDbfs=-40))
        save(root/'playback.json',dict(startedUtc=utc(),streams=records,sampleRate=48000))
        time.sleep(15)
        subprocess.run([sys.executable,str(Path(__file__).with_name('run.py')),'arm','--bundle',str(args.bundle)],check=True)
        baseline=snapshot();previous=baseline;start=time.monotonic()
        save(root/'snapshot-0000.json',baseline)
        if not baseline['tracing'] or baseline['audit']['clientTiming']['flightFrozenAtNs']:
            raise RuntimeError('Recorder froze during playback startup; collect this event before rerunning')
        state=dict(state,state='RUNNING',baselineXruns=baseline['audit']['xruns'],processPid=baseline['audit']['processPid'],checkIntervalSeconds=900)
        save(root/'status.json',state);print(json.dumps(state),flush=True)
        index=0
        while True:
            remaining=args.seconds-(time.monotonic()-start)
            # Local waiting only: no device polls between the requested intervals.
            time.sleep(min(900,max(0,remaining)))
            current=snapshot();index+=1
            save(root/f'snapshot-{index:04}.json',current)
            errors=check(baseline,previous,current)
            if any(p.poll() is not None for p in children): errors.append('Playback exited early')
            elapsed=time.monotonic()-start
            device_elapsed=(current['audit']['monotonicNs']-baseline['audit']['monotonicNs'])/1e9
            done=elapsed>=args.seconds and device_elapsed>=args.seconds
            state.update(state='FAIL' if errors else ('PASS' if done else 'RUNNING'),updatedUtc=utc(),elapsedSeconds=elapsed,deviceElapsedSeconds=device_elapsed,xrunDelta=current['audit']['xruns']-baseline['audit']['xruns'],errors=errors,samples=index+1)
            save(root/'status.json',state);print(json.dumps(state),flush=True)
            if errors or done: break
            previous=current
    except Exception as error:
        state.update(state='INVALID',error=str(error),updatedUtc=utc())
        save(root/'status.json',state);print(json.dumps(state),flush=True)
    finally:
        stop.set()
        for child in children:
            if child.poll() is None: child.terminate()
        for child in children:
            try: child.wait(timeout=5)
            except subprocess.TimeoutExpired: child.kill();child.wait()
        for log in logs: log.close()
        try:
            if snapshot()['tracing']: run.remote('stop')
            data=run.remote('collect')
            capture=root/'capture';capture.mkdir()
            (capture/'capture.tar.gz').write_bytes(data)
            with tarfile.open(fileobj=io.BytesIO(data),mode='r:gz') as archive: archive.extractall(capture,filter='data')
            subprocess.run([sys.executable,str(Path(__file__).with_name('analyze.py')),str(capture)],stdout=(root/'analysis.log').open('w'),check=True)
        except Exception as error:
            state['captureError']=str(error);save(root/'status.json',state)
    return 0 if state['state']=='PASS' else 1

if __name__=='__main__': sys.exit(main())
