# ============================================================
#  Nova OS — build & run
# ============================================================

TARGET      := x86_64-nova_os
MODE        ?= debug

BUILD_DIR   := build
ISO_DIR     := iso
KERNEL      := $(BUILD_DIR)/nova-os.bin
ISO         := $(BUILD_DIR)/nova-os.iso
LINKER      := boot/linker.ld
RUST_LIB    := target/$(TARGET)/$(MODE)/libnova_os.a

ifeq ($(MODE),release)
CARGO_FLAGS := --release
else
CARGO_FLAGS :=
endif

QEMU        := qemu-system-x86_64

# Disque virtuel du systeme de fichiers (phase 4). Branche sur un
# controleur AHCI : c'est le pilote de src/ahci.rs qui le pilote.
# Il persiste entre deux lancements — c'est ce qui permet de verifier
# qu'un volume deja formate est remonte tel quel au demarrage suivant.
# Les images vivent hors de build/ : `make clean` ne doit jamais detruire
# un systeme de fichiers. Pour repartir a zero, c'est `make disk-reset`,
# qui le dit explicitement.
DISK_DIR    := disks
DISK        := $(DISK_DIR)/nova-disk.img
DISK_SIZE   ?= 2048

# Trois supports amovibles vierges, un par format d'export (spec 4.4).
# Nova OS ne les formate pas — il n'ecrase jamais un support qu'il ne
# reconnait pas — et s'en sert comme cibles d'export. Les images
# produites sont lisibles par l'hote : c'est `make check-export` qui le
# verifie, avec les outils du systeme.
MEDIA_FAT   := $(DISK_DIR)/nova-export.img
MEDIA_EXT4  := $(DISK_DIR)/nova-ext4.img
MEDIA_NTFS  := $(DISK_DIR)/nova-ntfs.img
MEDIA_SIZE  ?= 96
MEDIA       := $(MEDIA_FAT) $(MEDIA_EXT4) $(MEDIA_NTFS)

# L'ordre des ports AHCI fixe l'ordre de detection, donc quel support
# recoit quel format : le pilote balaye les ports dans l'ordre croissant
# et l'autotest attribue les formats dans l'ordre de la liste.
QEMU_DISK   := -drive id=novadisk,file=$(DISK),format=raw,if=none \
               -device ich9-ahci,id=ahci \
               -device ide-hd,drive=novadisk,bus=ahci.0 \
               -drive id=novafat,file=$(MEDIA_FAT),format=raw,if=none \
               -device ide-hd,drive=novafat,bus=ahci.1 \
               -drive id=novaext4,file=$(MEDIA_EXT4),format=raw,if=none \
               -device ide-hd,drive=novaext4,bus=ahci.2 \
               -drive id=novantfs,file=$(MEDIA_NTFS),format=raw,if=none \
               -device ide-hd,drive=novantfs,bus=ahci.3

# Carte reseau et peripherique audio (phase 6.1.4 et 6.1.5).
#
# Le modele e1000 est celui que pilote src/drivers/e1000.rs. Le mode
# "user" de QEMU fournit un serveur DHCP en 10.0.2.2 et un relais DNS en
# 10.0.2.3 : c'est ce qui rend la phase 6.2 verifiable sans toucher au
# reseau de la machine hote.
#
# `-device AC97` exige un audiodev meme muet ; "none" produit un
# controleur complet dont les registres repondent, sans sortie sonore.
#
# La carte par defaut est virtio-net (src/drivers/virtio_net.rs) : c'est
# elle qui permet de depasser les 125 Mo/s d'une e1000. Le noyau essaie
# virtio-net, puis l'e1000. `make run QEMU_NIC=e1000` eprouve le repli.
QEMU_NIC    ?= virtio-net-pci
QEMU_NET    := -netdev user,id=net0 -device $(QEMU_NIC),netdev=net0
#
# Le backend est PipeWire par defaut, celui de CachyOS : c'est ce qui fait
# entendre la sortie et donne un microphone a l'entree du controleur.
# `make run QEMU_AUDIODEV=none` redonne une carte muette et sourde (utile
# en SSH), `pa` passe par PulseAudio.
QEMU_AUDIODEV ?= pipewire
#
# Micro et haut-parleur de l'hote. L'invite ne voit qu'une carte AC'97
# virtuelle : c'est QEMU qui la branche sur un peripherique reel, et c'est
# donc ici qu'on le choisit. Vides, ce sont la source et la sortie par
# defaut de PipeWire. Les noms se lisent avec `make audio-devices` :
#   make run QEMU_AUDIO_IN=alsa_input.usb-... QEMU_AUDIO_OUT=alsa_output.pci-...
# Le flux peut aussi etre deplace a chaud depuis pavucontrol ou qpwgraph.
QEMU_AUDIO_IN  ?=
QEMU_AUDIO_OUT ?=
comma := ,
QEMU_AUDIO_TARGETS := $(if $(QEMU_AUDIO_IN),$(comma)in.name=$(QEMU_AUDIO_IN))$(if $(QEMU_AUDIO_OUT),$(comma)out.name=$(QEMU_AUDIO_OUT))
QEMU_AUDIO  := -audiodev $(QEMU_AUDIODEV),id=snd0$(QEMU_AUDIO_TARGETS) -device AC97,audiodev=snd0

# Controleur xHCI et tablette USB — c'est ce que pilote src/usb/.
#
# `usb-tablet` est un pointeur **absolu** : il transmet une position, pas
# un deplacement. QEMU n'a donc pas besoin de confisquer le pointeur de
# l'hote pour l'envoyer, et le curseur de l'invite suit celui de l'hote
# sans qu'on ait a cliquer dans la fenetre.
#
# `qemu-xhci` plutot que `nec-usb-xhci` : le premier est le modele que
# QEMU maintient, et il expose des ports USB 2 et USB 3 sur le meme
# controleur — de quoi eprouver les deux chemins de reinitialisation de
# port.
QEMU_USB    := -device qemu-xhci,id=xhci -device usb-tablet,bus=xhci.0

# La souris emulee est une PS/2 **relative** : elle n'emet un deplacement
# que lorsque le pointeur de l'hote est capture par la fenetre. Sans
# capture, l'invite ne recoit rien du tout — le curseur reste immobile a
# l'ecran alors que le pilote, le compositeur et le module curseur
# fonctionnent parfaitement, ce qui est le pire des diagnostics.
#
# `grab-on-hover=on` capture des que le pointeur survole la fenetre, sans
# clic prealable. La capture se relache avec Ctrl+Alt+G.
#
# Un `-device usb-tablet` donnerait un pointeur absolu qui n'a pas besoin
# d'etre capture, mais il faudrait alors une pile USB dans le noyau — ce
# qui n'est au programme d'aucune phase.
#
# `zoom-to-fit=off` : agrandir la fenetre doit agrandir l'ecran de l'invite,
# pas etirer l'image. Avec virtio-gpu, GTK annonce la nouvelle taille au
# peripherique, et src/graphics/virtio_gpu/ change de mode pour la suivre.
# `gl=on` quand la carte graphique est la variante 3D (QEMU_GPU) : QEMU
# exige alors un affichage OpenGL.
QEMU_DISPLAY = -display gtk,grab-on-hover=on,zoom-to-fit=off$(if $(QEMU_GPU),$(comma)gl=on)

# Le GPU de l'hote (phase 13). Le seul chemin de l'invite vers la carte
# de l'hote est virtio-gpu 3D : `virtio-vga-gl` (la meme VGA pour GRUB,
# plus les contextes 3D), `venus=true` (le protocole que parle le pilote
# de Nova : Vulkan serialise), `blob=true` et `hostmem=8G` (la fenetre de
# memoire partagee ou l'hote projette la memoire du GPU que Nova lit et
# ecrit sans copie).
#
# Le noeud de rendu : celui de la carte NVIDIA quand il y en a une, sinon
# le premier. Nomme par son chemin PCI (/dev/dri/by-path), stable d'un
# demarrage de l'hote a l'autre, contrairement a renderD128/129. Il ne sert
# qu'a l'affichage sans fenetre (egl-headless) ; le GPU que Nova utilise,
# il le choisit lui-meme parmi ceux que l'hote expose (le dedie d'abord).
#
# `make run QEMU_GPU=` redonne la carte 2D (`-vga virtio`) et le rendu
# logiciel -- c'est aussi le repli automatique quand l'hote n'a pas de
# noeud de rendu.
QEMU_RENDER ?= $(shell for r in /sys/class/drm/renderD*; do [ -e "$$r" ] || continue; \
	  p=$$(basename $$(readlink -f $$r/device)); \
	  [ "$$(cat $$r/device/vendor 2>/dev/null)" = 0x10de ] && { echo /dev/dri/by-path/pci-$$p-render; exit; }; \
	  first=$${first:-/dev/dri/by-path/pci-$$p-render}; done; echo $$first)
QEMU_GPU    ?= $(if $(QEMU_RENDER),-device virtio-vga-gl$(comma)hostmem=8G$(comma)blob=true$(comma)venus=true)

# Carte graphique sans le GPU : virtio-vga, une VGA classique pour GRUB et
# la console de demarrage, plus virtio-gpu, qui dit au noyau la taille de
# la fenetre hote et le previent quand elle change. `-vga std` redonne la
# carte d'avant et le mode fixe de GRUB -- c'est la variante "pilote
# absent" ; `-vga none` reste la variante "aucun ecran" (avec QEMU_GPU=).
QEMU_VGA    ?= $(if $(QEMU_GPU),,-vga virtio)

# Sans fenetre : la variante 3D exige un contexte OpenGL, fourni sans
# ecran par egl-headless sur le noeud de rendu.
QEMU_HEADLESS = $(if $(QEMU_GPU),-display egl-headless$(comma)rendernode=$(QEMU_RENDER),-display none)

# Memoire de l'invite. 16 Gio depuis le 23 septembre 2026 (demande de
# l'utilisateur : plusieurs videos et onglets a la fois). Au-dela de
# 3 Gio, le firmware place le reste au-dessus du trou PCI, apres 4 Gio --
# memory::extend_window etend la fenetre physique pour l'atteindre (64 Gio
# au plus). `make run QEMU_MEM=512M` redonne l'ancienne configuration.
QEMU_MEM    ?= 16G

# Coeurs de l'invite pour run, run-full, run-nogui et log. Huit, le
# maximum du noyau (gdt::MAX_CPUS) : un .cus n'a que la moitie haute des
# coeurs (spec 7.2), soit quatre pour le navigateur. Sur un seul coeur,
# une video 720p perdait neuf images sur dix. `make run QEMU_SMP=1`
# redonne la variante monocoeur ; run-smp reste a 4 pour ses essais.
QEMU_SMP    ?= 8

# La racine du sous-systeme Linux n'a plus de disque a elle : ses fichiers
# sont dans le volume Nova, sous /apps/web-browser/root, ecrits par
# `make webroot` (tools/nova-fs.py import). Un fichier du navigateur est
# un noeud du systeme de fichiers comme un autre.

# Acceleration materielle quand l'hote l'offre : sous emulation pure (TCG)
# chaque instruction de l'invite est traduite, et le Web Browser met des
# minutes a demarrer. `make run QEMU_ACCEL="-accel tcg"` redonne l'ancien
# comportement.
#
# `kvm-pit.lost_tick_policy=discard` : par defaut, QEMU fait reinjecter
# par KVM les tics perdus du PIT, et sur un processeur AMD cette
# reinjection interdit l'AVIC -- la livraison materielle des interruptions
# entre coeurs. Chaque IPI passait alors par l'hyperviseur : un aller-retour
# entre deux threads coutait 17 us contre 6 avec l'AVIC (3 sur l'hote,
# tools/ports/tests/sysbench.c). Nova ne se sert du PIT que pour etalonner
# son minuteur, sans interruption : il n'a pas de tic a perdre.
QEMU_ACCEL  ?= $(if $(wildcard /dev/kvm),-accel kvm -cpu host -global kvm-pit.lost_tick_policy=discard,-accel tcg)

QEMU_FLAGS  = -cdrom $(ISO) -boot d -m $(QEMU_MEM) $(QEMU_ACCEL) $(QEMU_VGA) $(QEMU_GPU) $(QEMU_DISK) $(QEMU_NET) $(QEMU_AUDIO) $(QEMU_USB)

# Mesure de debit reseau (net::bench). L'hote sert un fichier de
# BENCH_MIB Mio sur 127.0.0.1:8000, que l'invite voit en 10.0.2.2 par le
# mode user de QEMU. Le noyau le telecharge une fois le reseau configure,
# parce que fw_cfg lui transmet la cible ; sans cette option, rien ne se
# passe au demarrage. Resultat : les lignes [debit] de build/bench.log.
# Le script arrete QEMU des que la mesure est ecrite.
BENCH_MIB   ?= 512
# Par defaut, la meme emulation que `make run` (TCG). Pour mesurer sous
# KVM : make bench-net BENCH_ACCEL="-accel kvm -cpu host"
BENCH_ACCEL ?=
BENCH_PORT  ?= 8000

.PHONY: test test-smp1 test-novga test-all dist version venus venus-check shaders kmd-test webroot nova-libc disk-migrate disk-lifs print-qemu-flags print-qemu-headless bench-net all kernel iso audio-devices run run-full run-smp run-nogui log debug debug-int clean check check-ascii police racines re prev unprev cargo-build disk disk-reset check-export run-netdump run-nonet

all: iso

# --- compilation ---

ASM_SRC     := boot/boot.asm boot/switch.asm boot/trampoline.asm boot/syscall_entry.asm boot/usermode.asm boot/fault.asm boot/vmtest.asm
ASM_OBJ     := $(BUILD_DIR)/boot.o $(BUILD_DIR)/switch.o $(BUILD_DIR)/trampoline.o $(BUILD_DIR)/syscall_entry.o $(BUILD_DIR)/usermode.o $(BUILD_DIR)/fault.o $(BUILD_DIR)/vmtest.o

$(BUILD_DIR):
	mkdir -p $(BUILD_DIR)

$(BUILD_DIR)/%.o: boot/%.asm | $(BUILD_DIR)
	nasm -f elf64 $< -o $@

# `make PILE=1` : noyau avec pointeurs de cadre, qui imprime sa pile
# d'appels a la panique (adresses a traduire par llvm-addr2line). A
# refaire sans l'option ensuite : tout le noyau est recompile.
PILE ?=
PILE_FLAGS := $(if $(PILE),-C force-frame-pointers=yes --cfg nova_pile,)

# Commit the kernel is built from, printed in the boot banner next to the
# version of Cargo.toml (src/version.rs). Absent outside a Git checkout.
NOVA_COMMIT ?= $(shell git rev-parse --short HEAD 2>/dev/null)

cargo-build:
	$(if $(NOVA_COMMIT),NOVA_COMMIT=$(NOVA_COMMIT)) RUSTFLAGS="$(PILE_FLAGS)" cargo build $(CARGO_FLAGS)

$(KERNEL): $(ASM_OBJ) cargo-build $(LINKER)
	ld.lld -n -T $(LINKER) -o $@ $(ASM_OBJ) $(RUST_LIB)
	@grub-file --is-x86-multiboot2 $@ && echo "[ok] header Multiboot2 valide"

kernel: $(KERNEL)

# --- image ISO ---

$(ISO): $(KERNEL) $(ISO_DIR)/boot/grub/grub.cfg
	cp $(KERNEL) $(ISO_DIR)/boot/nova-os.bin
	grub-mkrescue -o $@ $(ISO_DIR) 2> /dev/null
	@echo "[ok] ISO generee : $@"

iso: $(ISO)

# --- disque virtuel ---

$(DISK_DIR):
	mkdir -p $(DISK_DIR)

# Le disque systeme (phase 14) : une table GPT, p1 LIFS "nova-systeme"
# marquee systeme -- la racine --, p2 LIFS "nova-donnees" de
# $(DATA_SIZE) Mio -- A:/. Cree par l'hote : le noyau ne formate plus rien
# de lui-meme (DECISIONS, 14.0.1), et c'est ce qui permet d'y ecrire la
# racine Linux du navigateur (500 Mio) sans demarrer (tools/lifs.py).
DATA_SIZE   ?= 256
$(DISK): | $(DISK_DIR)
	@tools/lifs.py create $@ --size $(DISK_SIZE) --data $(DATA_SIZE)
	@echo "[ok] disque systeme de $(DISK_SIZE) Mio partitionne et formate en LIFS : $@"

$(MEDIA_FAT) $(MEDIA_EXT4) $(MEDIA_NTFS): | $(DISK_DIR)
	@dd if=/dev/zero of=$@ bs=1M count=$(MEDIA_SIZE) status=none
	@echo "[ok] support amovible de $(MEDIA_SIZE) Mio cree : $@"

disk: $(DISK) $(MEDIA)

# Efface les images et recree un disque systeme vierge (GPT + LIFS) : le
# prochain demarrage recreera l'arborescence. Indispensable pour rejouer
# un premier boot.
disk-reset:
	rm -f $(DISK) $(MEDIA)
	$(MAKE) disk

# Verifie les trois images produites par l'export (spec 4.6) avec les
# outils de l'hote : c'est la seule preuve qu'elles sont reellement
# lisibles ailleurs que dans Nova OS. Chaque verificateur est facultatif ;
# son absence est signalee, elle n'echoue pas le make.
check-export: $(MEDIA)
	@echo "=== FAT32 : $(MEDIA_FAT) ==="
	@fsck.vfat -n $(MEDIA_FAT) 2>&1 || true
	@mdir -/ -i $(MEDIA_FAT) :: 2>/dev/null | head -40 \
	  || echo "(mtools absent — pacman -S mtools pour lister)"
	@echo
	@echo "=== ext4 : $(MEDIA_EXT4) ==="
	@if command -v fsck.ext4 > /dev/null; then \
	   fsck.ext4 -fn $(MEDIA_EXT4) || true; \
	   debugfs -R "ls -l /" $(MEDIA_EXT4) 2>/dev/null | head -20 || true; \
	 else echo "(e2fsprogs absent — pacman -S e2fsprogs)"; fi
	@echo
	@echo "=== NTFS : $(MEDIA_NTFS) ==="
	@if command -v ntfsfix > /dev/null; then \
	   ntfsfix -n $(MEDIA_NTFS) || true; \
	   ntfsls -l $(MEDIA_NTFS) 2>/dev/null | head -20 || true; \
	 else echo "(ntfs-3g absent — pacman -S ntfs-3g)"; fi

# --- exécution ---

# 1.6.1 + 1.6.3 : fenêtre graphique VGA, console série redirigée vers le terminal
run: $(ISO) $(DISK) $(MEDIA)
	@$(if $(QEMU_GPU),echo "[qemu] GPU de l'hote par virtio-gpu 3D (Venus), noeud $(QEMU_RENDER)",echo "[qemu] pas de GPU de l'hote (aucun noeud de rendu, ou QEMU_GPU=) : carte 2D et rendu logiciel")
	$(QEMU) $(QEMU_FLAGS) -smp $(QEMU_SMP) $(QEMU_DISPLAY) -serial stdio

# Micros et sorties de l'hote, sous les noms qu'attendent QEMU_AUDIO_IN et
# QEMU_AUDIO_OUT.
audio-devices:
	@echo "--- entrees (QEMU_AUDIO_IN) ---"
	@pactl list short sources | awk '$$2 !~ /\.monitor$$/ {print "  " $$2}'
	@echo "--- sorties (QEMU_AUDIO_OUT) ---"
	@pactl list short sinks | awk '{print "  " $$2}'
	@echo "--- par defaut ---"
	@echo "  entree : $$(pactl get-default-source)"
	@echo "  sortie : $$(pactl get-default-sink)"

# Plein ecran : l'ecran de l'OS prend la taille du moniteur hote.
# Ctrl+Alt+F rend la fenetre.
run-full: $(ISO) $(DISK) $(MEDIA)
	$(QEMU) $(QEMU_FLAGS) -smp $(QEMU_SMP) $(QEMU_DISPLAY) -full-screen -serial stdio

# Série uniquement, sans fenêtre : pratique en SSH ou pour itérer vite
run-nogui: $(ISO) $(DISK) $(MEDIA)
	$(QEMU) $(QEMU_FLAGS) -smp $(QEMU_SMP) $(QEMU_HEADLESS) -serial stdio

# Journal de boot conservé dans un fichier
log: $(ISO) $(DISK) $(MEDIA)
	$(QEMU) $(QEMU_FLAGS) -smp $(QEMU_SMP) $(QEMU_HEADLESS) -serial file:$(BUILD_DIR)/boot.log
	@echo "--- $(BUILD_DIR)/boot.log ---"
	@cat $(BUILD_DIR)/boot.log

# --- débogage ---

# QEMU attend une connexion GDB sur :1234 avant d'exécuter la moindre instruction.
# Dans un autre terminal : gdb build/nova-os.bin -ex "target remote :1234"
debug: $(ISO) $(DISK) $(MEDIA)
	$(QEMU) $(QEMU_FLAGS) $(QEMU_DISPLAY) -serial stdio -s -S

# Trace les interruptions et les resets CPU : indispensable pour diagnostiquer
# un triple fault (écran noir ou reboot en boucle).
debug-int: $(ISO) $(DISK) $(MEDIA)
	$(QEMU) $(QEMU_FLAGS) $(QEMU_DISPLAY) -serial stdio -no-reboot -no-shutdown -d int,cpu_reset

# Genere un faux binaire "version precedente" pour tester l'entree 1.7.4
prev: $(KERNEL)
	cp $(KERNEL) $(ISO_DIR)/boot/nova-os-prev.bin
	grub-mkrescue -o $(ISO) $(ISO_DIR) 2> /dev/null
	@echo "[ok] version precedente ajoutee a l'ISO"

# Retire la version precedente pour retester le message d'absence
unprev:
	rm -f $(ISO_DIR)/boot/nova-os-prev.bin
	$(MAKE) iso

# --- utilitaires ---

check: $(KERNEL) check-ascii
	@echo "--- symboles non resolus (doit etre vide) ---"
	@nm -u $(KERNEL) || true
	@echo "--- instructions SSE (doit etre vide) ---"
	@objdump -d $(KERNEL) | grep -E 'movaps|movups|xmm' || true
	@echo "--- segments ---"
	@readelf -l $(KERNEL) | grep -A2 LOAD

# Un caractere non-ASCII dans une chaine affichee devient un carre plein a
# l'ecran, en mode texte comme sur le framebuffer. La regle est ecrite dans
# CLAUDE.md depuis le debut ; elle n'a tenu qu'a partir du jour ou elle a
# ete verifiee. Les commentaires et la documentation ne sont pas touches.
check-ascii:
	@echo "--- chaines affichees hors ASCII (doit etre vide) ---"
	@python3 tools/ascii-check.py

# Regenere src/graphics/font.rs depuis la description en ASCII-art.
police:
	python3 tools/generer-police.py

# Regenere l'encodeur Venus du pilote GPU (src/gpu/venus/protocol.rs) depuis
# le registre Vulkan (phase 13). Les trois fichiers XML viennent de
# venus-protocol, au commit que tools/gen-venus.py epingle, et leurs sommes
# y sont verifiees. `make venus-check` compare ensuite l'encodeur, octet
# par octet, a l'encodeur de reference de venus-protocol.
VENUS_COMMIT := $(shell sed -n "s/^VENUS_PROTOCOL_COMMIT = '\(.*\)'/\1/p" tools/gen-venus.py)
VENUS_XML    := vk.xml VK_EXT_command_serialization.xml VK_MESA_venus_protocol.xml
venus:
	@mkdir -p $(BUILD_DIR)/venus/xml
	@for f in $(VENUS_XML); do \
	  [ -f $(BUILD_DIR)/venus/xml/$$f ] || curl -sfL -o $(BUILD_DIR)/venus/xml/$$f \
	    https://gitlab.freedesktop.org/virgl/venus-protocol/-/raw/$(VENUS_COMMIT)/xmls/$$f || exit 1; \
	done
	python3 tools/gen-venus.py --xml $(BUILD_DIR)/venus/xml --out src/gpu/venus/protocol.rs

venus-check: venus
	tools/venus-check/check.sh

# Recompile les shaders du noyau (src/gpu/shaders/*.spv) : l'autotest du GPU
# et le compositeur sur le GPU. Le SPIR-V est versionne, comme la police :
# la construction du noyau n'a besoin ni de glslang ni du reseau. Paquet
# Arch : glslang.
# Le module d'essai du chargeur de pilotes GPU (src/gpu/module/test-gpu.kmd,
# phase 13.4.1b), versionne comme le SPIR-V : un pilote qui cherche une
# carte inexistante et se retire. Drapeaux : ceux que tools/kmd/nova_kmd.h
# impose a tout module.
KMD_CFLAGS := -ffreestanding -fPIC -fvisibility=hidden -fno-plt -nostdlib -shared \
	-mno-red-zone -mgeneral-regs-only -fno-stack-protector -O2 -Wall -Wextra \
	-Wl,-e,nova_kmd_init -Wl,-z,max-page-size=4096 -Wl,--hash-style=gnu
kmd-test:
	@mkdir -p $(BUILD_DIR)/kmd
	gcc $(KMD_CFLAGS) -Itools/kmd -o $(BUILD_DIR)/kmd/test-gpu.so tools/kmd/test-gpu.c
	@if readelf -rW $(BUILD_DIR)/kmd/test-gpu.so | grep -q "R_X86_64_[A-QS-Z]"; then \
	  echo "[!!] relocation autre que R_X86_64_RELATIVE"; exit 1; fi
	python3 tools/nova-kmd.py $(BUILD_DIR)/kmd/test-gpu.so src/gpu/module/test-gpu.kmd \
	  --name test-gpu --vendor "Nova OS" --signature system

# SPIR-V 1.5 (Vulkan 1.2) : en 1.6, glslang traduit `discard` par
# OpDemoteToHelperInvocation, capacite que la validation de Nova refuse
# (spec 6.8). En 1.5, c'est OpKill, sans capacite.
SHADERS := $(wildcard src/gpu/shaders/*.vert src/gpu/shaders/*.frag)
shaders:
	@for f in $(SHADERS); do glslangValidator -V --target-env vulkan1.2 -o $$f.spv $$f >/dev/null || exit 1; echo "[ok] $$f.spv"; done

# Regenere les racines de confiance du navigateur (src/web/roots.rs) depuis
# le magasin de certificats de la machine de developpement.
racines:
	python3 tools/generer-racines.py

# Ne touche pas a $(DISK_DIR) : le disque porte un systeme de fichiers,
# l'effacer est une operation distincte (`make disk-reset`).
clean:
	cargo clean
	rm -rf $(BUILD_DIR)
	rm -f $(ISO_DIR)/boot/nova-os.bin

# Lance QEMU avec 4 coeurs pour tester le SMP
run-smp: $(ISO) $(DISK) $(MEDIA)
	$(QEMU) $(QEMU_FLAGS) $(QEMU_DISPLAY) -smp 4 -serial stdio

# Enregistre toutes les trames dans build/net.pcap, lisible par
# Wireshark ou `tcpdump -r`. C'est la seule facon de constater qu'une
# negociation DHCP ou une requete DNS est reellement partie sur le cable :
# la sortie serie dit ce que la pile croit avoir fait, le fichier dit ce
# qui a circule.
run-netdump: $(ISO) $(DISK) $(MEDIA)
	$(QEMU) $(QEMU_FLAGS) \
	  -object filter-dump,id=dump0,netdev=net0,file=$(BUILD_DIR)/net.pcap \
	  $(QEMU_HEADLESS) -serial stdio
	@echo "[ok] trames enregistrees dans $(BUILD_DIR)/net.pcap"

# Sans carte reseau ni audio : verifie que le demarrage se passe bien
# quand le materiel est absent. Un pilote doit se declarer absent, pas
# echouer.
#
# `-nic none` est indispensable : sans -netdev, QEMU ajoute d'office une
# e1000, et cette cible demarrait en realite avec une carte reseau.
run-nonet: $(ISO) $(DISK) $(MEDIA)
	$(QEMU) -cdrom $(ISO) -boot d -m $(QEMU_MEM) -nic none $(QEMU_DISK) -display none -serial stdio

bench-net: $(ISO) $(DISK) $(MEDIA)
	BUILD_DIR=$(BUILD_DIR) BENCH_MIB=$(BENCH_MIB) BENCH_PORT=$(BENCH_PORT) BENCH_DISPLAY="$(QEMU_HEADLESS)" \
	  tools/bench-net.sh $(QEMU) $(BENCH_ACCEL) $(QEMU_FLAGS) -smp 4

# --- tests automatiques et distribution ---

# Version unique du systeme : le champ de Cargo.toml (src/version.rs).
VERSION := $(shell sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)

version:
	@echo $(VERSION)

# `make test` : demarre l'ISO sans fenetre et juge la sortie serie
# (tools/boot-test.py) -- tous les verdicts d'autotest verts, aucune
# panique, aucun ECHEC, aucun BOOT_ERR_. QEMU s'arrete des que le dernier
# verdict est imprime. Le disque est le sien (build/test/), neuf a chaque
# premier demarrage : le disque de travail (disks/) n'est jamais touche.
# Deux demarrages sur le meme disque : le second doit le retrouver
# (CLAUDE.md, "installation idempotente").
TEST_DIR     := $(BUILD_DIR)/test
TEST_DISK    := $(TEST_DIR)/nova-disk.img
TEST_SMP     ?= 4
TEST_MEM     ?= 2G
TEST_TIMEOUT ?= 900
TEST_VGA     ?= -vga virtio
# L'ISO eprouvee : celle de build/ par defaut ; la release eprouve la
# sienne (make test TEST_ISO=build/dist/nova-os-X.iso), sans la refaire.
TEST_ISO     ?= $(ISO)
TEST_FLAGS    = -cdrom $(TEST_ISO) -boot d -m $(TEST_MEM) $(QEMU_ACCEL) $(TEST_VGA) \
                -drive id=novadisk,file=$(TEST_DISK),format=raw,if=none \
                -device ich9-ahci,id=ahci -device ide-hd,drive=novadisk,bus=ahci.0 \
                -netdev user,id=net0 -device virtio-net-pci,netdev=net0 \
                -audiodev none,id=snd0 -device AC97,audiodev=snd0 \
                -device qemu-xhci,id=xhci -device usb-tablet,bus=xhci.0 \
                -display none -no-reboot
TEST_EXTRA   ?=

test: $(TEST_ISO)
	@mkdir -p $(TEST_DIR)
	@rm -f $(TEST_DISK)
	@tools/lifs.py create $(TEST_DISK) --size 1024 --data 128 > /dev/null
	python3 tools/boot-test.py --timeout $(TEST_TIMEOUT) --log $(TEST_DIR)/boot1-smp$(TEST_SMP).log $(TEST_EXTRA) \
	  -- $(QEMU) $(TEST_FLAGS) -smp $(TEST_SMP)
	python3 tools/boot-test.py --timeout $(TEST_TIMEOUT) --log $(TEST_DIR)/boot2-smp$(TEST_SMP).log $(TEST_EXTRA) \
	  --expect '\[fs\]   demarrage n 2 ' \
	  -- $(QEMU) $(TEST_FLAGS) -smp $(TEST_SMP)

# Les variantes de CLAUDE.md : un seul coeur, aucun ecran.
test-smp1:
	$(MAKE) --no-print-directory test TEST_SMP=1

test-novga:
	$(MAKE) --no-print-directory test TEST_VGA="-vga none" TEST_EXTRA=--no-screen

test-all: test test-smp1 test-novga

# `make dist` : ce que publie une release (.github/workflows/release.yml).
# Le noyau y est depouille de ses informations de debogage (92 -> 8 Mio) ;
# celui de build/ les garde, pour llvm-addr2line. Le disque systeme est
# une image GPT + LIFS vierge, compressee : `xz -d` puis
# `-drive file=nova-disk.img,...` (README).
DIST_DIR     := $(BUILD_DIR)/dist
DIST_NAME    := nova-os-$(VERSION)
OBJCOPY      ?= $(shell command -v objcopy || command -v llvm-objcopy)

dist: $(KERNEL)
	rm -rf $(DIST_DIR)
	mkdir -p $(DIST_DIR)/iso/boot/grub
	$(OBJCOPY) --strip-debug $(KERNEL) $(DIST_DIR)/iso/boot/nova-os.bin
	cp $(ISO_DIR)/boot/grub/grub.cfg $(DIST_DIR)/iso/boot/grub/grub.cfg
	grub-mkrescue -o $(DIST_DIR)/$(DIST_NAME).iso $(DIST_DIR)/iso 2> /dev/null
	cp $(DIST_DIR)/iso/boot/nova-os.bin $(DIST_DIR)/$(DIST_NAME)-kernel.bin
	tools/lifs.py create $(DIST_DIR)/$(DIST_NAME)-disk.img --size 2048 --data 256 > /dev/null
	xz -T0 -9 -f $(DIST_DIR)/$(DIST_NAME)-disk.img
	rm -rf $(DIST_DIR)/iso
	cd $(DIST_DIR) && sha256sum $(DIST_NAME).iso $(DIST_NAME)-kernel.bin $(DIST_NAME)-disk.img.xz > SHA256SUMS
	@echo "[ok] distribution $(VERSION) dans $(DIST_DIR)"
	@ls -la $(DIST_DIR)

# Reconstruction complete. Deux sous-make plutot que `re: clean all` :
# make resout tout le graphe de dependances avant d'executer la moindre
# recette, il considere donc les .o comme a jour au moment ou `clean` les
# supprime, et l'edition de liens echoue sur des fichiers absents.
re:
	$(MAKE) clean
	$(MAKE) all

# Image racine du sous-systeme Linux : paquets Alpine et programmes de
# test, dans une archive tar (tools/ports/webroot.sh). Par defaut, ce
# qu'il faut au Web Browser : Firefox d'Alpine pour ses bibliotheques, le
# fork lui-meme (ajoute par le script s'il est construit), les polices,
# les certificats, et tzdata pour le fuseau horaire (/etc/localtime).
# NOVA_AUTOSTART_BROWSER=1 make webroot : le navigateur s'ouvre a la
# connexion (image de test).
WEBROOT_PACKAGES ?= alpine-baselayout alpine-release musl busybox firefox-esr font-dejavu ca-certificates tzdata

webroot: | $(DISK)
	tools/ports/webroot.sh $(WEBROOT_PACKAGES)
	tools/lifs.py import $(DISK) build/ports/webroot /apps/web-browser/root
	$(MAKE) --no-print-directory nova-libc

# Phase 12 : la bibliotheque C Nova (musl dont les appels systeme sont les
# appels Nova de la spec 6.7), installee dans le volume a
# /sys/runtimes/c/libc.so, ou le noyau la prend pour chaque programme de
# l'interface Nova. COPYRIGHT est la licence MIT de musl, qui l'accompagne.
nova-libc: | $(DISK)
	tools/ports/nova-libc/build.sh
	tools/lifs.py put $(DISK) build/ports/nova-libc/out/libc.so /sys/runtimes/c/libc.so
	tools/lifs.py put $(DISK) build/ports/nova-libc/out/COPYRIGHT /sys/runtimes/c/COPYRIGHT

# Reprend un disque au format 1 (noms de 56 caracteres) dans un volume au
# format courant, sans rien perdre : l'ancien est garde en .v1.
disk-migrate:
	@test -f $(DISK) || { echo "aucun disque a reprendre"; exit 1; }
	cp $(DISK) $(DISK).v1
	tools/nova-fs.py migrate $(DISK).v1 $(DISK) --size $(DISK_SIZE)
	@echo "[ok] ancien disque conserve : $(DISK).v1"

# 14.5.2 -- Reprend un disque NovaFS (d'avant la phase 14) dans un disque
# GPT + LIFS neuf : meme arborescence, memes proprietaires et droits.
# L'ancien n'est que lu, et garde en .novafs.
disk-lifs:
	@test -f $(DISK) || { echo "aucun disque a reprendre"; exit 1; }
	mv $(DISK) $(DISK).novafs
	tools/lifs.py from-novafs $(DISK).novafs $(DISK) --size $(DISK_SIZE) --data $(DATA_SIZE)
	tools/lifs.py check $(DISK)
	@echo "[ok] ancien disque conserve : $(DISK).novafs"

# Drapeaux QEMU effectifs, pour les scripts de test sans fenetre ; et
# l'affichage sans fenetre qui va avec (egl-headless avec le GPU).
print-qemu-flags:
	@echo $(QEMU_FLAGS)

print-qemu-headless:
	@echo $(QEMU_HEADLESS)
