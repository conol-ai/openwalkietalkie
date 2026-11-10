/* Small browser adaptations of the firmware's three local Arcade games.
   Source: src/ui/state/arcade/{mod.rs,games.rs}. No audio capture or host traffic. */
(function () {
  'use strict';
  window.OWTArcade = function (options) {
    var $ = function (id) { return document.getElementById(id); };
    var canvas = $('arcadeCanvas'), ctx = canvas.getContext('2d');
    var menu = $('arcadeMenu'), play = $('arcadePlay'), status = $('arcadeStatus');
    var buttons = Array.from(menu.querySelectorAll('button'));
    var selection = 0, game = null, phase = 'menu', score = 0, best = {};
    var frame = null, previous = 0, accumulated = 0;
    var bird, pipes, snake, direction, pendingDirection, food, ship, enemies, bullets, ticks;
    var names = { flappy: 'Flappy Bird', snake: 'Snake', raiden: 'Raiden' };
    function select(i) {
      selection = Math.max(0, Math.min(2, i));
      buttons.forEach(function (b, at) { b.classList.toggle('is-selected', at === selection); });
    }
    function stopLoop() { cancelAnimationFrame(frame); frame = null; previous = 0; }
    function updateStatus() {
      status.textContent = names[game] + ' · ' + score + ' pts · ' + (phase === 'over' ? 'GAME OVER' : phase === 'paused' ? 'PAUSED' : phase === 'ready' ? 'READY' : 'PLAYING');
      $('arcadeStart').textContent = phase === 'playing' ? 'Pause' : phase === 'paused' ? 'Resume' : phase === 'over' ? 'Play again' : 'Start game';
    }
    function newFood() {
      var empty = [];
      for (var y = 0; y < 12; y++) for (var x = 0; x < 16; x++) if (!snake.some(function (s) { return s.x === x && s.y === y; })) empty.push({ x: x, y: y });
      return empty.length ? empty[Math.floor(Math.random() * empty.length)] : null;
    }
    function prepare(key) {
      stopLoop(); game = key; phase = 'ready'; score = 0; ticks = 0; accumulated = 0;
      bird = { y: 100, v: 0 }; pipes = [{ x: 310, gap: 60 + Math.random() * 60, passed: false }];
      snake = [{ x: 6, y: 6 }, { x: 5, y: 6 }, { x: 4, y: 6 }]; direction = pendingDirection = 0; food = newFood();
      ship = 160; enemies = []; bullets = [];
      menu.hidden = true; play.hidden = false;
      updateStatus(); draw();
      options.log('in', names[key] + ' · local browser game · PTT controls play');
    }
    function over() {
      phase = 'over'; best[game] = Math.max(best[game] || 0, score); stopLoop();
      options.log('in', names[game] + ' · ' + score + ' points · best ' + best[game]); options.blip(220, 0.12);
      updateStatus(); draw();
    }
    function start() {
      if (phase === 'playing') { deactivate(); return; }
      if (phase === 'over') prepare(game);
      phase = 'playing'; previous = 0; accumulated = 0; updateStatus(); frame = requestAnimationFrame(loop);
    }
    function deactivate() { if (phase === 'playing') { phase = 'paused'; stopLoop(); updateStatus(); draw(); } }
    function back() { stopLoop(); phase = 'menu'; menu.hidden = false; play.hidden = true; select(selection); }
    function loop(time) {
      if (phase !== 'playing') return;
      if (!previous) previous = time;
      accumulated += Math.min(50, time - previous); previous = time;
      var step = game === 'snake' ? 135 : 1000 / 60;
      while (accumulated >= step && phase === 'playing') { update(); accumulated -= step; }
      draw();
      if (phase === 'playing') frame = requestAnimationFrame(loop);
    }
    function update() {
      ticks++;
      if (game === 'snake') {
        direction = pendingDirection;
        var dx = [1, 0, -1, 0][direction], dy = [0, 1, 0, -1][direction];
        var head = { x: snake[0].x + dx, y: snake[0].y + dy };
        var eating = food && head.x === food.x && head.y === food.y;
        var body = eating ? snake : snake.slice(0, -1);
        if (head.x < 0 || head.x >= 16 || head.y < 0 || head.y >= 12 || body.some(function (s) { return s.x === head.x && s.y === head.y; })) { over(); return; }
        snake.unshift(head);
        if (eating) { score += 10; food = newFood(); options.blip(760, 0.03); updateStatus(); if (!food) over(); }
        else snake.pop();
      } else if (game === 'flappy') {
        bird.v += 0.16; bird.y += bird.v;
        if (ticks % 115 === 0) pipes.push({ x: 330, gap: 55 + Math.random() * 65, passed: false });
        pipes.forEach(function (p) {
          p.x -= 1.6;
          if (!p.passed && p.x + 32 < 70) { p.passed = true; score++; updateStatus(); }
          if (p.x < 82 && p.x + 32 > 60 && (bird.y - 8 < p.gap || bird.y + 8 > p.gap + 88)) over();
        });
        pipes = pipes.filter(function (p) { return p.x > -40; });
        if (bird.y < 8 || bird.y > 232) over();
      } else {
        if (ticks % 13 === 0) bullets.push({ x: ship, y: 200 });
        if (ticks % 45 === 0) enemies.push({ x: 16 + Math.random() * 288, y: -15 });
        bullets.forEach(function (b) { b.y -= 5; });
        enemies.forEach(function (enemy) {
          enemy.y += 1 + Math.min(score / 150, 1.5);
          bullets.forEach(function (b) { if (!enemy.hit && !b.hit && Math.abs(b.x - enemy.x) < 15 && Math.abs(b.y - enemy.y) < 13) { enemy.hit = b.hit = true; score += 10; updateStatus(); } });
          if (!enemy.hit && enemy.y > 192 && enemy.y < 223 && Math.abs(enemy.x - ship) < 20) over();
          if (!enemy.hit && enemy.y > 245) over();
        });
        bullets = bullets.filter(function (b) { return b.y > 0 && !b.hit; });
        enemies = enemies.filter(function (e) { return !e.hit; });
      }
    }
    function draw() {
      ctx.fillStyle = '#070d19'; ctx.fillRect(0, 0, 320, 240);
      ctx.fillStyle = '#172737';
      for (var x = 0; x < 320; x += 20) for (var y = 0; y < 240; y += 20) ctx.fillRect(x, y, 1, 1);
      if (game === 'snake') {
        snake.forEach(function (s, i) { ctx.fillStyle = i ? '#41b97d' : '#b4f68c'; ctx.fillRect(s.x * 20 + 2, s.y * 20 + 2, 16, 16); });
        if (food) { ctx.fillStyle = '#ff9964'; ctx.fillRect(food.x * 20 + 5, food.y * 20 + 5, 10, 10); }
      } else if (game === 'flappy') {
        ctx.fillStyle = '#3fc395'; pipes.forEach(function (p) { ctx.fillRect(p.x, 0, 32, p.gap); ctx.fillRect(p.x, p.gap + 88, 32, 240); });
        ctx.fillStyle = '#ffca61'; ctx.fillRect(60, bird.y - 8, 20, 16); ctx.fillStyle = '#fff'; ctx.fillRect(74, bird.y - 6, 6, 6); ctx.fillStyle = '#ff8454'; ctx.fillRect(80, bird.y, 6, 4);
      } else if (game === 'raiden') {
        ctx.fillStyle = '#72e8ed'; ctx.fillRect(ship - 4, 194, 8, 27); ctx.fillRect(ship - 16, 210, 32, 8);
        ctx.fillStyle = '#ffca61'; bullets.forEach(function (b) { ctx.fillRect(b.x - 2, b.y, 4, 9); });
        ctx.fillStyle = '#ff697c'; enemies.forEach(function (e) { ctx.fillRect(e.x - 12, e.y, 24, 8); ctx.fillRect(e.x - 4, e.y - 6, 8, 24); });
      }
      if (phase !== 'playing') {
        ctx.fillStyle = '#000a'; ctx.fillRect(0, 85, 320, 70); ctx.fillStyle = '#f1e8c9'; ctx.font = '18px monospace'; ctx.textAlign = 'center';
        ctx.fillText(phase === 'ready' ? 'PRESS START' : phase === 'paused' ? 'PAUSED' : 'GAME OVER', 160, 115);
        ctx.font = '12px monospace'; ctx.fillText(phase === 'over' ? 'Score ' + score + ' · try again' : 'PTT / ENTER / screen to play', 160, 138);
      }
    }
    function action(key) {
      if (phase === 'menu') {
        if (key === 'left' || key === 'right') select(selection + (key === 'left' ? -1 : 1));
        else if (key === 'enter' || key === 'ptt') prepare(buttons[selection].dataset.game);
        return key !== 'cancel';
      }
      if (key === 'cancel') { if (phase === 'playing') deactivate(); else back(); return true; }
      if (key === 'enter') { start(); return true; }
      if (key === 'ptt') { if (phase !== 'playing') start(); if (game === 'flappy') bird.v = -3.5; return true; }
      if (key === 'left' || key === 'right') {
        var d = key === 'left' ? -1 : 1;
        if (game === 'snake' && pendingDirection === direction) pendingDirection = (direction + d + 4) % 4;
        if (game === 'raiden') ship = Math.max(16, Math.min(304, ship + d * 18));
        draw();
      }
      return true;
    }
    buttons.forEach(function (b, i) { b.addEventListener('click', function () { select(i); prepare(b.dataset.game); }); });
    $('arcadeStart').addEventListener('click', start); $('arcadeBack').addEventListener('click', back);
    canvas.addEventListener('pointerdown', function (e) {
      if (game === 'flappy' || phase !== 'playing') action('ptt');
      else action(e.clientX < canvas.getBoundingClientRect().left + canvas.clientWidth / 2 ? 'left' : 'right');
    });
    document.addEventListener('visibilitychange', function () { if (document.hidden) deactivate(); });
    select(0);
    return { action: action, deactivate: deactivate };
  };
})();
