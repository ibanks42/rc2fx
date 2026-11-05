#!/usr/bin/env python3
"""Two-hour six-channel qualification; device reads only at the requested checkpoints."""
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
    output=run.ssh('printf "boot="; cat /proc/sys/kernel/random/boot_id; printf "uptime="; cat /proc/uptime; printf "home="; cat /run/rc2fx/home; printf "tracing="; cat /sys/kernel/debug/tracing/tracing_on; printf "audit="; cat /run/rc2fx/audit.json').decode()
    fields=dict(line.split('=',1) for line in output.splitlines() if '=' in line)
    return dict(hostUtc=utc(),bootId=fields['boot'],uptime=float(fields['uptime'].split()[0]),home=fields['home'],tracing=int(fields['tracing']),audit=json.loads(fields['audit']))


def check(base, previous, current, expect_trace=True):
    a, b = current['audit'], base['audit']
    errors=[]
    if current['home']!='/opt/rc2fx': errors.append('Runtime is not installed')
    if current['bootId']!=base['bootId'] or a['processPid']!=b['processPid']: errors.append('Mixer restarted')
    if not 0<=current['uptime']-a['monotonicNs']/1e9<5: errors.append('Stale telemetry')
    if a['callbacks']<=previous['audit']['callbacks']: errors.append('Callbacks stopped')
    for key in ['xruns','dspFaults','capacityFailures','planErrors','controlServerErrors']:
        if a[key]!=b[key]: errors.append(key+' changed')
    before_workers={w['id']:w['worker'] for w in b.get('workers',[])}
    for worker in a.get('workers',[]):
        prior=before_workers.get(worker['id'])
        if prior is None:errors.append('Suppression worker identity changed');continue
        for key in ['inputDrops','outputDrops','missedDeadlines','staleResults']:
            if worker['worker'].get(key)!=prior.get(key):errors.append('Suppression '+key+' changed')
    for key in ['late','stalls']:
        if a['helper'][key]!=b['helper'][key]: errors.append('helper '+key+' changed')
    flight=a['clientTiming']
    if expect_trace and flight['flightToken']!=b['clientTiming']['flightToken']: errors.append('Flight recorder token changed')
    if expect_trace and (flight['flightFrozenAtNs'] or not current['tracing']): errors.append('Fault recorder froze')
    return errors


def main():
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--out', type=Path, required=True)
    parser.add_argument('--no-trace',action='store_true')
    parser.add_argument('--deep',action='store_true')
    parser.add_argument('--avoid-driver',action='store_true')
    parser.add_argument('--bundle',type=Path,required=True)
    parser.add_argument('--seconds', type=int, default=7200)
    parser.add_argument('--interval',type=int,default=30)
    # Interval may equal duration for a baseline/end-only qualification.
    args=parser.parse_args()
    if args.seconds<=0 or args.interval<=0:parser.error('Duration and interval must be positive')
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
        if not args.no_trace:
            subprocess.run([sys.executable,str(Path(__file__).with_name('run.py')),'arm','--bundle',str(args.bundle)]+(['--deep'] if args.deep else []),check=True)
        if args.avoid_driver:
            audit=json.loads(run.ssh('cat /run/rc2fx/audit.json'))
            tids=[r['tid'] for r in audit['clientTiming']['registrations'] if r['name'] in ('usb1out','usb1-chat-out') and r['tid']]
            if len(tids)!=2:raise RuntimeError('USB output identity is ambiguous')
            run.remote('avoid-driver',*tids)
        baseline=snapshot();previous=baseline;start=time.monotonic()
        save(root/'snapshot-0000.json',baseline)
        if not args.no_trace and (not baseline['tracing'] or baseline['audit']['clientTiming']['flightFrozenAtNs']):
            raise RuntimeError('Recorder froze during playback startup; collect this event before rerunning')
        if args.no_trace and baseline['audit']['clientTiming']['enabled']:raise RuntimeError('Control run requires optional timing probe disabled at audio start')
        state=dict(state,state='RUNNING',baselineXruns=baseline['audit']['xruns'],processPid=baseline['audit']['processPid'],checkIntervalSeconds=args.interval)
        save(root/'status.json',state);print(json.dumps(state),flush=True)
        index=0
        while True:
            remaining=args.seconds-(time.monotonic()-start)
            # Local waiting only: no device polls between the requested intervals.
            time.sleep(min(args.interval,max(0,remaining)) + (5 if remaining<=args.interval else 0))
            current=snapshot();index+=1
            save(root/f'snapshot-{index:04}.json',current)
            errors=check(baseline,previous,current,not args.no_trace)
            if any(p.poll() is not None for p in children): errors.append('Playback exited early')
            elapsed=time.monotonic()-start
            device_elapsed=(current['audit']['monotonicNs']-baseline['audit']['monotonicNs'])/1e9
            if elapsed>=args.seconds and device_elapsed<args.seconds:errors.append('Device telemetry did not cover the requested duration')
            done=elapsed>=args.seconds and device_elapsed>=args.seconds
            state.update(state='FAIL' if errors else ('PASS' if done else 'RUNNING'),updatedUtc=utc(),elapsedSeconds=elapsed,deviceElapsedSeconds=device_elapsed,xrunDelta=current['audit']['xruns']-baseline['audit']['xruns'],errors=errors,samples=index+1)
            save(root/'status.json',state);print(json.dumps(state),flush=True)
            if errors or done: break
            previous=current
    except KeyboardInterrupt:
        state.update(state='CANCELLED',error='Run interrupted',updatedUtc=utc())
        save(root/'status.json',state)
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
        if not args.no_trace:
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
