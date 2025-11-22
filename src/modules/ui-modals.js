// --- HELPERS INTERNI ---
export function setupModals(state, refreshCallback, updateBannerCallback) {
  
  // --- EDIT MODAL ---
  const modalEdit = document.getElementById("modal-edit");
  const btnEditBanner = document.getElementById("btn-edit-banner");
  const btnConfirmEdit = document.getElementById("btn-confirm-edit");
  const editNameInput = document.getElementById("edit-name-input");
  const editIconInput = document.getElementById("edit-icon-input");

  // Open Edit
  btnEditBanner.addEventListener('click', () => {
    if (!state.activeServerId) return;
    const server = state.serverList.find(s => s.id === state.activeServerId);
    editNameInput.value = server.name;
    editIconInput.value = server.icon;
    modalEdit.style.display = 'flex';
  });

  // Save Edit
  btnConfirmEdit.addEventListener('click', () => {
    if (!state.activeServerId) return;
    const server = state.serverList.find(s => s.id === state.activeServerId);
    if (server) {
      // Update Local Data (Mock logic for now)
      server.name = editNameInput.value;
      server.icon = editIconInput.value;
      
      refreshCallback(); // Re-render sidebar
      updateBannerCallback(server); // Update banner immediately
      modalEdit.style.display = 'none';
    }
  });

  // --- NEW SERVER MODAL ---
  const modalNew = document.getElementById("modal-new");
  const btnAddServer = document.getElementById("btn-add-server");
  const stepSelection = document.getElementById("step-selection");
  const stepForm = document.getElementById("step-form");
  const btnCreateConfirm = document.getElementById("btn-create-confirm");
  const btnBackStep = document.getElementById("btn-back-step");
  
  // Wizard State
  let creationType = null;

  btnAddServer.addEventListener('click', () => {
    creationType = null;
    document.getElementById("new-server-name").value = "";
    stepSelection.style.display = 'block';
    stepForm.style.display = 'none';
    btnBackStep.style.display = 'none';
    btnCreateConfirm.style.display = 'none';
    modalNew.style.display = 'flex';
  });

  // Wizard Steps Logic
  document.querySelectorAll('.selection-card').forEach(card => {
    card.addEventListener('click', () => {
      creationType = card.getAttribute('data-type');
      
      stepSelection.style.display = 'none';
      stepForm.style.display = 'block';
      btnBackStep.style.display = 'inline-block';
      btnCreateConfirm.style.display = 'inline-block';

      // Toggle dynamic fields based on creationType...
      // (You can copy the exact logic from your original main.js here)
      document.getElementById("fields-create").style.display = creationType === 'create' ? 'block' : 'none';
      document.getElementById("fields-import").style.display = creationType === 'import' ? 'block' : 'none';
      document.getElementById("fields-remote").style.display = creationType === 'remote' ? 'block' : 'none';
    });
  });

  btnBackStep.addEventListener('click', () => {
    stepForm.style.display = 'none';
    stepSelection.style.display = 'block';
    btnBackStep.style.display = 'none';
    btnCreateConfirm.style.display = 'none';
  });

  // Create Confirm
  btnCreateConfirm.addEventListener('click', () => {
    // ... logic to create object ...
    // For now, just close
    modalNew.style.display = 'none';
    console.log("Create logic to be implemented in Rust backend integration");
  });

  // Generic Close for all modals
  document.querySelectorAll('.btn-close-icon, .btn-secondary').forEach(btn => {
    btn.addEventListener('click', () => {
      modalEdit.style.display = 'none';
      modalNew.style.display = 'none';
    });
  });
}