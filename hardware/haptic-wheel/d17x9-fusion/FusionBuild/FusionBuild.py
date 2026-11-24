"""Native Fusion reconstruction of the approved HW17-R0 geometry.

Run inside Autodesk Fusion: exec(compile(open(PATH).read(), PATH, 'exec')); run(None)
All lengths in source are mm; Fusion internal coordinates are cm.
Creates a NEW document; never changes the source R0 or an existing user document.
"""
import adsk.core, adsk.fusion
import json, math, traceback, time
from pathlib import Path

HERE = Path(__file__).resolve().parent.parent if '__file__' in globals() else Path('/Users/tony/projects/conol/openmicrodial/hardware/haptic-wheel/d17x9-fusion')
SOURCE = HERE / 'source'
G = json.loads((SOURCE / 'design.json').read_text(encoding='utf-8'))['geometry']
META = {p['name']: p for p in json.loads((SOURCE / 'verification.json').read_text(encoding='utf-8'))['parts']}
LOG = HERE / 'build.log'
V = adsk.core.ValueInput
P = adsk.core.Point3D
F = adsk.fusion.FeatureOperations
PARTS = []

def log(s):
    with LOG.open('a', encoding='utf-8') as f: f.write(str(s)+'\n')

def pt(x=0, y=0, z=0): return P.create(x/10, y/10, z/10)
def val(x): return V.createByString(x if isinstance(x, str) else f'{x:.12g} mm')
def ev(x): return design.unitsManager.evaluateExpression(x, 'mm')*10 if isinstance(x,str) else x
def oc(items):
    c=adsk.core.ObjectCollection.create()
    for x in items: c.add(x)
    return c

def component(name, parent):
    o=parent.occurrences.addNewComponent(adsk.core.Matrix3D.create())
    o.component.name=name
    return o

def part(key, cn, parent):
    o=component(key+' '+cn,parent)
    c=o.component
    c.partNumber='HW17-R0-'+key
    m=META[key]
    c.description=m['material']+' | '+m['note']
    c.attributes.add('HW17','source_part',key)
    c.attributes.add('HW17','status','Concept prototype geometry; not a production release')
    PARTS.append((key,o))
    log('PART '+key)
    return c

def sketch(c, name):
    s=c.sketches.add(c.xYConstructionPlane)
    s.name=name+' / XY'
    return s

def circle(s, radius, x=0, y=0):
    r=ev(radius)
    a=s.sketchCurves.sketchCircles.addByCenterRadius(pt(x,y),r/10)
    # A named diameter constraint exposes useful dimensions in the native model.
    if isinstance(radius,str):
        d=s.sketchDimensions.addDiameterDimension(a,pt(x+r,y+r))
        d.parameter.expression='2*('+radius+')'
    return a

def rectangle(s,x,y,dx,dy,angle=0):
    a=math.radians(angle)
    def q(px,py): return pt(px*math.cos(a)-py*math.sin(a),px*math.sin(a)+py*math.cos(a))
    pp=[q(x,y),q(x+dx,y),q(x+dx,y+dy),q(x,y+dy)]
    for i in range(4): s.sketchCurves.sketchLines.addByTwoPoints(pp[i],pp[(i+1)%4])

def extrude(c,s,z,h,name,operation=F.NewBodyFeatureOperation,ring_profile=False):
    if ring_profile:
        profiles=[p for p in s.profiles if p.profileLoops.count==2]
    else:
        profiles=list(s.profiles)
    if not profiles: raise RuntimeError('No profile: '+name)
    inp=c.features.extrudeFeatures.createInput(oc(profiles),operation)
    inp.startExtent=adsk.fusion.OffsetStartDefinition.create(val(z))
    inp.setOneSideExtent(adsk.fusion.DistanceExtentDefinition.create(val(h)),adsk.fusion.ExtentDirections.PositiveExtentDirection)
    if operation==F.CutFeatureOperation: inp.participantBodies=list(c.bRepBodies)
    e=c.features.extrudeFeatures.add(inp)
    e.name=name
    s.isVisible=False
    return e

def ring(c,ro,ri,z,h,name,operation=F.NewBodyFeatureOperation):
    s=sketch(c,name);circle(s,ro)
    if ev(ri)>0: circle(s,ri)
    return extrude(c,s,z,h,name,operation,ev(ri)>0)

def disk(c,r,z,h,name,operation=F.NewBodyFeatureOperation,x=0,y=0):
    s=sketch(c,name);circle(s,r,x,y)
    return extrude(c,s,z,h,name,operation)

def box(c,x,y,z,dx,dy,h,name,operation=F.NewBodyFeatureOperation,angle=0):
    s=sketch(c,name);rectangle(s,x,y,dx,dy,angle)
    return extrude(c,s,z,h,name,operation)

def sector(c,ro,ri,a0,a1,z,h,name,operation=F.NewBodyFeatureOperation):
    s=sketch(c,name)
    t0,t1=math.radians(a0),math.radians(a1)
    outer=s.sketchCurves.sketchArcs.addByCenterStartSweep(pt(),pt(ro*math.cos(t0),ro*math.sin(t0)),t1-t0)
    inner=s.sketchCurves.sketchArcs.addByCenterStartSweep(pt(),pt(ri*math.cos(t0),ri*math.sin(t0)),t1-t0)
    lines=s.sketchCurves.sketchLines
    lines.addByTwoPoints(outer.startSketchPoint,inner.startSketchPoint)
    lines.addByTwoPoints(outer.endSketchPoint,inner.endSketchPoint)
    return extrude(c,s,z,h,name,operation)

def color(c,hexcolor):
    lib=app.materialLibraries.itemByName('Fusion Appearance Library')
    appbase=lib.appearances.itemByName('Paint - Enamel Glossy (White)')
    name='HW17 '+hexcolor
    a=design.appearances.itemByName(name)
    if not a:
        a=design.appearances.addByCopy(appbase,name)
        prop=adsk.core.ColorProperty.cast(a.appearanceProperties.itemById('opaque_albedo'))
        h=hexcolor.lstrip('#');prop.value=adsk.core.Color.create(*(int(h[i:i+2],16) for i in (0,2,4)),255)
        rough=adsk.core.FloatProperty.cast(a.appearanceProperties.itemById('surface_roughness'))
        rough.value=.38
    for b in c.bRepBodies: b.appearance=a

def build():
    global rotor, fixed
    rotor=component('A 旋转总成 | ROTOR',root)
    fixed=component('B 固定总成 | STATOR + SENSOR',root)
    rc,fc=rotor.component,fixed.component
    c=part('01_grip_ring','POM 防滑外圈',rc)
    ring(c,'grip_od_mm/2','grip_id_mm/2','rotor_bottom_z_mm','top_z_mm-rotor_bottom_z_mm','外圈 OD17 ID16.4')
    s=sketch(c,'36 条浅防滑槽')
    for i in range(36):
        a=math.radians(i*10);circle(s,.18,8.56*math.cos(a),8.56*math.sin(a))
    extrude(c,s,1.35,7.30,'轴向防滑槽 36x',F.CutFeatureOperation)
    c=part('02_rotor_back_iron','低碳钢磁轭',rc)
    ring(c,'grip_id_mm/2','rotor_yoke_id_mm/2','rotor_bottom_z_mm','rotor_yoke_top_z_mm-rotor_bottom_z_mm','磁轭 0.50mm 径向壁厚')
    c=part('03_rotor_cap','铝端盖',rc)
    ring(c,'grip_id_mm/2','bearing_id_mm/2','rotor_yoke_top_z_mm','top_z_mm-rotor_yoke_top_z_mm','端盖')
    ring(c,'cap_spigot_od_mm/2','cap_spigot_id_mm/2','cap_spigot_bottom_z_mm','rotor_yoke_top_z_mm-cap_spigot_bottom_z_mm','胶接定位止口',F.JoinFeatureOperation)
    ring(c,'bearing_inner_abutment_od_mm/2','bearing_id_mm/2','bearing_upper_z_mm+bearing_width_mm','rotor_yoke_top_z_mm-bearing_upper_z_mm-bearing_width_mm','轴承内圈压肩',F.JoinFeatureOperation)
    ring(c,3.2,1.4,7.79,.31,'上挡环避让',F.CutFeatureOperation)
    c=part('04_rotor_shaft','非磁性精密轴 D2',rc)
    disk(c,'bearing_id_mm/2','bearing_lower_z_mm','top_z_mm-0.15 mm-bearing_lower_z_mm','轴身')
    disk(c,'bearing_inner_abutment_od_mm/2','bearing_lower_z_mm-0.3 mm',.3,'轴底肩',F.JoinFeatureOperation)
    pitch=360/G['magnet_poles']; half=(pitch-G['magnet_joint_gap_deg'])/2
    for i in range(G['magnet_poles']):
        pole='N' if i%2==0 else 'S'
        c=part(f'05_magnet_{i+1:02d}_{pole}',f'弧形磁钢 内侧{pole}极',rc)
        sector(c,7.65,6.75,i*pitch-half,i*pitch+half,'magnet_bottom_z_mm','magnet_height_mm','径向充磁 N35SH')
    c=part('06_stator_carrier','铝固定座',fc)
    disk(c,'carrier_base_od_mm/2',0,'carrier_base_thickness_mm','安装底盘')
    disk(c,'sensor_pocket_od_mm/2',-.01,'sensor_pcb_thickness_mm+0.01 mm','传感器板沉腔',F.CutFeatureOperation)
    disk(c,'bearing_od_mm/2','sensor_pcb_thickness_mm','carrier_base_thickness_mm-sensor_pcb_thickness_mm+0.01 mm','中心通腔',F.CutFeatureOperation)
    ring(c,'carrier_post_od_mm/2','bearing_od_mm/2','carrier_base_thickness_mm','bearing_upper_z_mm+bearing_width_mm-carrier_base_thickness_mm','轴承座筒',F.JoinFeatureOperation)
    ring(c,'bearing_od_mm/2','bearing_outer_stop_id_mm/2','bearing_lower_z_mm+bearing_width_mm','bearing_upper_z_mm-bearing_lower_z_mm-bearing_width_mm','轴承外圈间隔肩',F.JoinFeatureOperation)
    for i,a in enumerate(G['mount_angles_deg']):
        x=5.4*math.cos(math.radians(a));y=5.4*math.sin(math.radians(a))
        disk(c,'mount_boss_od_mm/2','carrier_base_thickness_mm','mount_boss_top_z_mm-carrier_base_thickness_mm',f'安装凸台 {i+1}',F.JoinFeatureOperation,x,y)
        disk(c,'mount_pilot_diameter_mm/2',-.01,'mount_boss_top_z_mm-0.24 mm',f'M1.4 底孔 {i+1} 无牙型',F.CutFeatureOperation,x,y)
    box(c,0,-1,0,7.1,2,.4,'传感器软排线通道 135deg',F.CutFeatureOperation,135)
    disk(c,.45,-.01,.62,'相线通孔 D0.9',F.CutFeatureOperation,0,4.7)
    c=part('07_stator_stack','12 槽叠片铁芯 H4',fc)
    ring(c,'stator_yoke_od_mm/2','stator_id_mm/2','stator_bottom_z_mm','stator_stack_mm','定子背轭')
    for i in range(12):
        box(c,3.9,-.4,'stator_bottom_z_mm',2.25,.8,'stator_stack_mm',f'齿 {i+1:02d}',F.JoinFeatureOperation,i*30)
        sector(c,6.5,6.1,i*30-11,i*30+11,'stator_bottom_z_mm','stator_stack_mm',f'齿靴 {i+1:02d}',F.JoinFeatureOperation)
    seq=['Ap','An','Bn','Bp','Cp','Cn','An','Ap','Bp','Bn','Cn','Cp']
    for i,phase in enumerate(seq):
        c=part(f'08_coil_{i+1:02d}_{phase}',f'{phase} 32匝绕组包络',fc)
        box(c,4.2,-.98,'coil_bottom_z_mm',1.75,1.96,'coil_top_z_mm-coil_bottom_z_mm','线圈外包络',angle=i*30)
        box(c,4.19,-.5,'stator_bottom_z_mm-0.1 mm',1.77,1.,'stator_stack_mm+0.2 mm','绕组齿间通道',F.CutFeatureOperation,i*30)
    for i,z in enumerate(['bearing_lower_z_mm','bearing_upper_z_mm']):
        c=part(f'09_bearing_{i+1}','MR52ZZ 轴承包络',fc)
        ring(c,'bearing_od_mm/2','bearing_id_mm/2',z,'bearing_width_mm','轴承 D2xD5x2.5 包络')
    c=part('10_inner_spacer','内圈隔垫',rc)
    ring(c,'bearing_inner_abutment_od_mm/2','bearing_id_mm/2','bearing_lower_z_mm+bearing_width_mm','bearing_upper_z_mm-bearing_lower_z_mm-bearing_width_mm','内隔垫 H0.10')
    for key,z in [('11_lower_outer_retainer','bearing_lower_z_mm-0.2 mm'),('12_upper_outer_retainer','bearing_upper_z_mm+bearing_width_mm')]:
        c=part(key,'轴承外圈挡环',fc)
        ring(c,'bearing_od_mm/2','bearing_outer_stop_id_mm/2',z,.2,'外圈挡环 H0.20')
    c=part('13_sensor_magnet','两极径向测角磁铁',rc)
    disk(c,'sensor_magnet_od_mm/2','sensor_magnet_bottom_z_mm','sensor_magnet_height_mm','D4x0.5 测角磁铁')
    c=part('14_sensor_flex','测角刚柔板包络',fc)
    disk(c,'sensor_pcb_od_mm/2',0,'sensor_pcb_thickness_mm','D5.5 加强区')
    box(c,0,-.8,0,6.8,1.6,.15,'传感器软排线包络',F.JoinFeatureOperation,135)
    c=part('15_MT6701_max_envelope','角度芯片最大包络',fc)
    box(c,-1.55,-1.55,.45,3.1,3.1,.8,'MT6701 QFN16 最大外形')
    c=part('16_sensor_decoupling','0201 去耦包络',fc)
    box(c,1.65,-.15,.45,.6,.3,.3,'100nF 0201 包络')
    for key,o in PARTS:
        assert o.component.bRepBodies.count==1,(key,o.component.bRepBodies.count)
        o.component.bRepBodies.item(0).name=key
        color(o.component,META[key]['color'])
        o.component.isOriginFolderLightBulbOn=False
        o.component.isSketchFolderLightBulbOn=False

def articulation():
    # All child parts stay fixed to their respective rotor / stator assembly.
    for key,o in PARTS: o.isGroundToParent=True
    fixed.isGrounded=True
    rotor.isGroundToParent=False
    origin=fixed.component.originConstructionPoint.createForAssemblyContext(fixed)
    geom=adsk.fusion.JointGeometry.createByPoint(origin)
    ji=root.asBuiltJoints.createInput(rotor,fixed,geom)
    ji.setAsRevoluteJointMotion(adsk.fusion.JointDirections.ZAxisJointDirection)
    joint=root.asBuiltJoints.add(ji)
    joint.name='滚轮旋转 Z | Revolute 360deg'
    # Verify this actually moves the whole rotor and restore the original pose.
    motion=adsk.fusion.RevoluteJointMotion.cast(joint.jointMotion)
    motion.rotationValue=math.radians(15)
    adsk.doEvents()
    trial=rotor.transform2.asArray()
    motion.rotationValue=0
    adsk.doEvents()
    assert abs(trial[0]-1)>.001, 'Revolute joint did not move rotor'
    (HERE/'joint-verification.json').write_text(json.dumps({'test_angle_deg':15,'rotor_transform_at_15deg':trial,'restored_angle_deg':0,'rotation_only':True},indent=2))
    root.isJointsFolderLightBulbOn=False

def sections():
    ss=design.analyses.sectionAnalyses
    axial=ss.add(ss.createInput(root.xZConstructionPlane,0))
    axial.name='01 轴向剖视 | AXIAL'
    axial.flip()
    axial.isLightBulbOn=False
    radial=ss.add(ss.createInput(root.xYConstructionPlane,.43))
    radial.name='02 气隙与绕组横剖 | RADIAL Z4.3mm'
    radial.isLightBulbOn=False
    return axial,radial

def inspect():
    report={'source':'HW17-R0','format':'Fusion native sketches and extrudes','parts':[],'warnings':[]}
    for key,o in PARTS:
        b=o.component.bRepBodies.item(0)
        bb=b.preciseBoundingBox
        coords=[*bb.minPoint.asArray(),*bb.maxPoint.asArray()]
        vv=b.physicalProperties.volume*1000
        expected=META[key]['volume_mm3']
        report['parts'].append({'name':key,'solid':b.isSolid,'volume_mm3':vv,'reference_volume_mm3':expected,'volume_error_mm3':vv-expected,'bbox_mm':[a*10 for a in coords]})
    for i in range(design.timeline.count):
        obj=design.timeline.item(i).entity
        if hasattr(obj,'healthState') and obj.healthState != adsk.fusion.FeatureHealthStates.HealthyFeatureHealthState:
            report['warnings'].append({'name':getattr(obj,'name',''),'message':getattr(obj,'errorOrWarningMessage','')})
    bb=root.preciseBoundingBox
    report['bbox_mm']=[v*10 for v in [*bb.minPoint.asArray(),*bb.maxPoint.asArray()]]
    report['timeline_count']=design.timeline.count
    report['part_count']=len(PARTS)
    report['max_volume_error_mm3']=max(abs(x['volume_error_mm3']) for x in report['parts'])
    (HERE/'verification.json').write_text(json.dumps(report,indent=2,ensure_ascii=False),encoding='utf-8')
    return report

def camera():
    cam=app.activeViewport.camera
    cam.cameraType=adsk.core.CameraTypes.OrthographicCameraType
    cam.eye=pt(23,-30,23);cam.target=pt(0,0,4.4)
    cam.upVector=adsk.core.Vector3D.create(0,0,1)
    cam.isFitView=True;cam.isSmoothTransition=False
    app.activeViewport.camera=cam
    app.activeViewport.refresh()
    adsk.doEvents()

def capture(name):
    opts=adsk.core.SaveImageFileOptions.create(str(HERE/name))
    opts.width=1800;opts.height=1400
    opts.isBackgroundTransparent=True
    app.activeViewport.saveAsImageFileWithOptions(opts)

def run(context):
    global app,design,root
    LOG.write_text('HW17 Fusion native build '+time.strftime('%Y-%m-%d %H:%M:%S')+'\n')
    PARTS.clear()
    try:
        app=adsk.core.Application.get()
        doc=app.documents.add(adsk.core.DocumentTypes.FusionDesignDocumentType)
        doc.name='HW17-R0 Fusion 原生电机滚轮'
        design=adsk.fusion.Design.cast(app.activeProduct)
        design.designIntent=adsk.fusion.DesignIntentTypes.HybridDesignIntentType
        design.designType=adsk.fusion.DesignTypes.ParametricDesignType
        design.unitsManager.distanceDisplayUnits=adsk.fusion.DistanceUnits.MillimeterDistanceUnits
        root=design.rootComponent
        root.description='12-slot / 14-pole custom brushless haptic wheel. Rotation only. Native Fusion reconstruction of 2026-10-24 R0; concept/prototype, not manufacturing release.'
        for key,x in G.items():
            if isinstance(x,(int,float)) and key.endswith('_mm'):
                design.userParameters.add(key,val(x),'mm','R0 geometry source; see build script for dependent sketch geometry')
        build()
        articulation()
        axial,radial=sections()
        report=inspect()
        log('VERIFIED '+json.dumps({k:v for k,v in report.items() if k!='parts'}))
        assert report['max_volume_error_mm3'] < .02, 'Volume comparison failed'
        assert not report['warnings'], 'Feature warnings found'
        exec((HERE/'FusionBuild'/'finish_views.py').read_text(encoding='utf-8'),globals())
        log('SUCCESS')
    except:
        log(traceback.format_exc())
        raise
