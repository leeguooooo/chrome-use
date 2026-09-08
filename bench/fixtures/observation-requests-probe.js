// Produce network activity without changing the fixture DOM or URL.
// --observe should keep the request evidence even though changed is false.
(async () => {
  await Promise.all(Array.from({ length: 25 }, (_, i) =>
    fetch('data:text/plain,' + String(i) + 'A'.repeat(420000)).then(response => response.text())
  ));
  return null;
})()
