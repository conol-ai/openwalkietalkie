"""Run after FusionBuild.run in the same Fusion Python namespace."""
def capture(name):
    opts=adsk.core.SaveImageFileOptions.create(str(HERE/name))
    opts.width=1800;opts.height=1400;opts.isBackgroundTransparent=True
    app.activeViewport.saveAsImageFileWithOptions(opts)

axial=design.analyses.sectionAnalyses.item(0)
radial=design.analyses.sectionAnalyses.item(1)
camera()
design.namedViews.add(app.activeViewport.camera,'01 总装轴测 | Assembly ISO')
app.activeViewport.setCurrentAsHome(True)
capture('assembled.png')
hidden=[]
for key,o in PARTS:
    if key in ['01_grip_ring','02_rotor_back_iron','03_rotor_cap'] or (key.startswith('05_magnet') and int(key.split('_')[2])>=9):
        o.isLightBulbOn=False;hidden.append(o)
camera();capture('internal.png')
for o in hidden:o.isLightBulbOn=True
axial.isLightBulbOn=True
camera();capture('section-axial.png')
axial.isLightBulbOn=False
radial.isLightBulbOn=True
cam=app.activeViewport.camera
cam.eye=pt(0,0,40);cam.target=pt(0,0,4.3);cam.upVector=adsk.core.Vector3D.create(0,1,0)
cam.isFitView=True;app.activeViewport.camera=cam
app.activeViewport.refresh();adsk.doEvents()
design.namedViews.add(app.activeViewport.camera,'02 轴向俯视 | Along Z')
capture('section-radial.png')
radial.isLightBulbOn=False
camera()
ex=design.exportManager
assert ex.execute(ex.createSTEPExportOptions(str(HERE/'HW17-R0-Fusion.step')))
assert ex.execute(ex.createFusionArchiveExportOptions(str(HERE/'HW17-R0-Fusion.f3d')))
log('VIEW EXPORT COMPLETE')
