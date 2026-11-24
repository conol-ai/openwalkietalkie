#!/usr/bin/env python3
"""Prepare critical local routes / import Freerouting SES and add ground planes."""
import pathlib, sys, json, math, shutil
import pcbnew as p
ROOT=pathlib.Path(__file__).resolve().parent
WORK=ROOT.parents[2]/'target/motor-electronics'
mm=p.FromMM
V=lambda xy:p.VECTOR2I(mm(xy[0]+100),mm(xy[1]+100))

def track(b,net,points,width=.2,layer=p.F_Cu):
    ni=b.FindNet(net)
    for a,z in zip(points,points[1:]):
        t=p.PCB_TRACK(b);t.SetStart(V(a));t.SetEnd(V(z));t.SetWidth(mm(width));t.SetLayer(layer);t.SetNet(ni);t.SetLocked(True);b.Add(t)

def prep(kind):
    name='HW17-'+kind+'-r0';folder=ROOT/'source'/name
    b=p.LoadBoard(str(folder/(name+'-placed.kicad_pcb')))
    if kind=='driver':
        track(b,'CP',[(27.535,22.8),(26.575,22.8),(25.775,22)])
        track(b,'VM',[(24.225,22),(24.225,24.3)],.3)
        track(b,'VM',[(27.535,23.2),(26.9,23.2),(26.6,23.5),(25.025,23.5),(24.225,24.3)],.2)
        track(b,'VM',[(27.535,23.2),(27.6,23.2),(28.2,23.8),(28.2,25.225)],.2)
        track(b,'VM',[(28.2,23.465),(28.2,23.8)],.2)
        for a in [(28.6,23.465),(30.465,22),(29,20.535)]:track(b,'GND',[a,(a[0],22) if a[0]!=30.465 else (29,22)],.2)
        track(b,'AVDD',[(30.465,21.6),(30.465,20.8),(31.1,20.165),(31.1,18.675),(30.425,18)],.2)
        track(b,'AVDD',[(29.4,20.535),(29.8,20.535),(30.065,20.8),(30.465,20.8)],.2)
        track(b,'AVDD',[(28.6,20.535),(28.6,19.5),(30.1,18),(30.425,18)],.2)
        track(b,'VM',[(24.225,24.3),(24.225,25.25),(22.475,27)],.4)
        track(b,'VM',[(22.475,27),(25.525,27),(28.525,30)],.5)
        track(b,'VM',[(39,8),(39,10.5),(36,13.5)],.65)
    else:
        f=b.FindFootprintByReference('J1')
        for a in f.Pads():
            n=int(a.GetNumber());offset=-.525+(n-1)*.35
            end=(-2.6/math.sqrt(2)+offset/math.sqrt(2),-2.6/math.sqrt(2)-offset/math.sqrt(2))
            start=(p.ToMM(a.GetPosition().x)-100,p.ToMM(a.GetPosition().y)-100)
            track(b,a.GetNetname(),[start,end],.12)
    # SaveBoard writes project state too: restore the reviewed rule settings afterwards.
    settings=(folder/(name+'.kicad_pro')).read_text()
    p.SaveBoard(str(folder/(name+'-pre.kicad_pcb')),b)
    (folder/(name+'-pre.kicad_pro')).write_text(settings)
    p.ExportSpecctraDSN(b,str(WORK/(kind+'.dsn')))
    dsn=WORK/(kind+'.dsn');s=dsn.read_text().replace('(clearance 25 (type smd_smd))','(clearance '+('120' if kind=='driver' else '100')+' (type smd_smd))')
    dsn.write_text(s)

def finish(kind):
    name='HW17-'+kind+'-r0';folder=ROOT/'source'/name
    settings=(folder/(name+'.kicad_pro')).read_text()
    b=p.LoadBoard(str(folder/(name+'-pre.kicad_pcb')))
    if not p.ImportSpecctraSES(b,str(WORK/(kind+'.ses'))):raise RuntimeError('SES import failed')
    # Bottom copper return plane. No copper plane or vias in the flex tail.
    if kind=='driver':
        for layer in [p.F_Cu,p.B_Cu]:
            z=p.ZONE(b);z.SetLayer(layer);z.SetNet(b.FindNet('GND'));z.SetLocalClearance(mm(.15));z.SetThermalReliefGap(mm(.2));z.SetThermalReliefSpokeWidth(mm(.3));z.SetMinThickness(mm(.15));z.SetPadConnection(p.ZONE_CONNECTION_FULL)
            poly=z.Outline();poly.NewOutline()
            for x,y in [(100.25,100.25),(149.75,100.25),(149.75,139.75),(100.25,139.75)]:poly.Append(mm(x),mm(y))
            b.Add(z)
    p.SaveBoard(str(folder/(name+'.kicad_pcb')),b)
    (folder/(name+'.kicad_pro')).write_text(settings)

if __name__=='__main__':
    {'prep':prep,'finish':finish}[sys.argv[1]](sys.argv[2])
