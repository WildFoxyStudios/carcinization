# Configuration file for VirtualBox, it creates a VirtualBox virtual machine

virtualbox: $(BUILD)/harddrive.img
	echo "Delete VM"
	-$(VBM) unregistervm Carcinization --delete; \
	if [ $$? -ne 0 ]; \
	then \
		if [ -d "$$HOME/VirtualBox VMs/Carcinization" ]; \
		then \
			echo "Carcinization directory exists, deleting..."; \
			$(RM) -rf "$$HOME/VirtualBox VMs/Carcinization"; \
		fi \
	fi
	echo "Delete Disk"
	-$(RM) harddrive.vdi
	echo "Create VM"
	$(VBM) createvm --name Carcinization --register
	echo "Set Configuration"
	$(VBM) modifyvm Carcinization --memory 2048
	$(VBM) modifyvm Carcinization --vram 32
	if [ "$(net)" != "no" ]; \
	then \
		$(VBM) modifyvm Carcinization --nic1 nat; \
		$(VBM) modifyvm Carcinization --nictype1 82540EM; \
		$(VBM) modifyvm Carcinization --cableconnected1 on; \
		$(VBM) modifyvm Carcinization --nictrace1 on; \
		$(VBM) modifyvm Carcinization --nictracefile1 "$(ROOT)/$(BUILD)/network.pcap"; \
	fi
	$(VBM) modifyvm Carcinization --uart1 0x3F8 4
	$(VBM) modifyvm Carcinization --uartmode1 file "$(ROOT)/$(BUILD)/serial.log"
	$(VBM) modifyvm Carcinization --usb off # on
	$(VBM) modifyvm Carcinization --keyboard ps2
	$(VBM) modifyvm Carcinization --mouse ps2
	$(VBM) modifyvm Carcinization --audio-driver $(VB_AUDIO)
	$(VBM) modifyvm Carcinization --audiocontroller hda
	$(VBM) modifyvm Carcinization --audioout on
	$(VBM) modifyvm Carcinization --nestedpaging on
	echo "Create Disk"
	$(VBM) convertfromraw $< $(BUILD)/harddrive.vdi
	echo "Attach Disk"
	$(VBM) storagectl Carcinization --name ATA --add sata --controller IntelAHCI --bootable on --portcount 1
	$(VBM) storageattach Carcinization --storagectl ATA --port 0 --device 0 --type hdd --medium $(BUILD)/harddrive.vdi
	echo "Run VM"
	$(VBM) startvm Carcinization
