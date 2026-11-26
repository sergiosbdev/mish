"""Crea Mish.terminal: copia del perfil 'Clear Dark' de Terminal que arranca mish."""
import plistlib, subprocess, sys, os
src = subprocess.run(["defaults", "export", "com.apple.Terminal", "-"], capture_output=True, check=True).stdout
prefs = plistlib.loads(src)
base = prefs["Window Settings"].get(sys.argv[1] if len(sys.argv) > 1 else "Clear Dark")
p = dict(base)
p["name"] = "Mish"
p["CommandString"] = "/bin/zsh -il -c 'exec ~/.cargo/bin/mish'"
p["RunCommandAsShell"] = False
p["shellExitAction"] = 1          # cerrar ventana al salir limpiamente
p["WindowTitle"] = "mish"
p["type"] = "Window Settings"
out = os.path.join(os.path.dirname(os.path.abspath(__file__)), "Mish.terminal")
with open(out, "wb") as f:
    plistlib.dump(p, f)
print(out)
