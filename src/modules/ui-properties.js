export function populatePropertiesPanel(props) {
  const p = props || {};
  const isTrue = (val) => val === 'true';

  // Text / Numbers
  document.getElementById('prop-motd').value = p['motd'] || "A Minecraft Server";
  document.getElementById('prop-max-players').value = p['max-players'] || "20";
  document.getElementById('prop-server-port').value = p['server-port'] || "25565";

  // Selects
  document.getElementById('prop-gamemode').value = p['gamemode'] || "survival";
  document.getElementById('prop-difficulty').value = p['difficulty'] || "normal";

  // Switches
  document.getElementById('prop-hardcore').checked = isTrue(p['hardcore']);
  document.getElementById('prop-pvp').checked = isTrue(p['pvp']);
  document.getElementById('prop-flight').checked = isTrue(p['allow-flight']);
  document.getElementById('prop-structures').checked = isTrue(p['generate-structures']);
  document.getElementById('prop-online-mode').checked = isTrue(p['online-mode']);
}