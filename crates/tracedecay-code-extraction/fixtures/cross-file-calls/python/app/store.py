from app.util import normalize


class Store:
    items = {}

    def add(self, key, value):
        self.items[normalize(key)] = value

    def get(self, key):
        return self.items.get(normalize(key))
