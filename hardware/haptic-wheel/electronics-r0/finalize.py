#!/usr/bin/env python3
"""Final local layout refinements and schematic/PCB net-name synchronization."""
import pathlib,json,sys,xml.etree.ElementTree as ET,importlib.util,math
import pcbnew as p
from route import ROOT,WORK,track,V,mm

def via(b,net,xy):
    v=p.PCB_VIA(b);v.SetPosition(V(xy));v.SetWidth(mm(.4));v.SetDrill(mm(.2));v.SetViaType(p.VIATYPE_THROUGH);v.SetLayerPair(p.F_Cu,p.B_Cu);v.SetNet(b.FindNet(net));b.Add(v)

def encoder(b):
    b.FindFootprintByReference('C1').SetPosition(V((0,-2.25)))
    tail=b.FindFootprintByReference('J1')
    tail.SetAttributes(tail.GetAttributes() | p.FP_EXCLUDE_FROM_BOM | p.FP_EXCLUDE_FROM_POS_FILES)
    ends={}
    for a in tail.Pads():
        n=int(a.GetNumber());net={1:'GND',2:'SDA',3:'SCL',4:'VDD'}[n];a.SetNet(b.FindNet(net))
        off=-.525+(n-1)*.35
        end=(-2.6/math.sqrt(2)+off/math.sqrt(2),-2.6/math.sqrt(2)-off/math.sqrt(2));ends[net]=end
        track(b,net,[(p.ToMM(a.GetPosition().x)-100,p.ToMM(a.GetPosition().y)-100),end],.12)
    def tr(n,pts,layer=p.F_Cu):track(b,n,pts,.12,layer)
    tr('GND',[ends['GND'],(-1.89249,-1.15),(-1.15,-1.15),(-.88,-1.45),(-.75,-1.45),(-.75,-1.35)])
    tr('SDA',[ends['SDA'],(-1.77758,-1.53),(-1.5,-1.53)])
    tr('SCL',[ends['SCL'],(-1.65245,-1.9),(-1.15,-1.9)])
    tr('VDD',[ends['VDD'],(-1.05,-2.32),(-.32,-2.32),(-.32,-2.3)])
    tr('VDD',[(-.32,-2.25),(-.32,-2.2),(.25,-1.63),(.25,-1.45),(.75,-1.45)])
    tr('VDD',[(.75,-1.45),(1.15,-1.05),(1.8,-1.05),(1.95,-.9),(1.95,.9),(1.4,1.45),(.75,1.45)])
    tr('GND',[(.32,-2.3),(.52,-2.1),(.9,-2.1)])
    for n,xy in [('SDA',(-1.5,-1.53)),('SCL',(-1.15,-1.9)),('GND',(-.75,-1.35)),('GND',(.9,-2.1)),('SDA',(-.25,2.0)),('SCL',(.5,2.1))]:via(b,n,xy)
    tr('GND',[(-.75,-1.35),(.15,-1.35),(.9,-2.1)],p.B_Cu)
    tr('SDA',[(-1.5,-1.53),(-1.5,1.4),(-.25,2.0)],p.B_Cu)
    tr('SCL',[(-1.15,-1.9),(-1.125,-1.875),(-1.125,.7),(.5,2.1)],p.B_Cu)
    tr('SDA',[(-.25,2.0),(-.25,1.45)])
    tr('SCL',[(.5,2.1),(.25,1.85),(.25,1.45)])

def main(kind):
    name='HW17-'+kind+'-r0';folder=ROOT/'source'/name
    settings=json.loads((folder/(name+'.kicad_pro')).read_text())
    rules=settings['board']['design_settings']['rules'];rules['min_hole_clearance']=.2
    b=p.LoadBoard(str(folder/(name+('-placed' if kind=='encoder' else '')+'.kicad_pcb')))
    if kind=='encoder':encoder(b)
    for t in b.GetTracks():
        if type(t)==p.PCB_TRACK:
            if t.GetWidth()<mm(.12):t.SetWidth(mm(.12))
            if kind=='driver':
                n=t.GetNetname().removeprefix('/')
                if n in ['PH_U','PH_V']:t.SetWidth(mm(.15 if p.ToMM(t.GetLength())<5 or n=='PH_U' else .25))
                elif n in ['PH_W','VIN_5V']:t.SetWidth(mm(.2))
    # KiCad local labels produce /net names and explicit no-connect net names.
    # Use the exported schematic netlist, including isolated EP/NC pads.
    root=ET.parse(WORK/(kind+'.net')).getroot()
    netmap={}
    for net in root.find('nets'):
        name_=net.attrib['name']
        old=name_.removeprefix('/')
        ni=b.FindNet(name_) or b.FindNet(old)
        if ni is None or ni.GetNetCode()==0:
            ni=p.NETINFO_ITEM(b,name_);b.Add(ni)
        else:ni.SetNetname(name_)
        for node in net.findall('node'):netmap[(node.attrib['ref'],node.attrib['pin'])]=ni
    for f in b.GetFootprints():
        for a in f.Pads():
            ni=netmap.get((f.GetReference(),a.GetNumber()))
            if ni is not None:a.SetNet(ni)
    p.SaveBoard(str(folder/(name+'.kicad_pcb')),b)
    (folder/(name+'.kicad_pro')).write_text(json.dumps(settings,indent=2))

if __name__=='__main__':main(sys.argv[1])
